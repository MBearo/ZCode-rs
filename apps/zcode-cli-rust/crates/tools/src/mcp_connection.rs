use super::mcp_config::Server;
use crate::contract::{ProcessCleanupFailure, ToolOutput};
use anyhow::{Context, Result, bail, ensure};
use futures_util::StreamExt;
use rmcp::{
    RoleClient,
    model::{ClientConfig, ClientRequest, ProtocolVersion},
    service::{
        ClientLifecycleMode, Peer, PeerRequestOptions, RunningService,
        serve_client_with_lifecycle_and_ct,
    },
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, process::Stdio, time::Duration};
use tokio::{
    process::{Child, Command},
    sync::Mutex,
};
use tokio_util::{
    codec::{FramedRead, FramedWrite},
    sync::CancellationToken,
};

type Service = RunningService<RoleClient, ClientConfig>;
pub(super) struct Connection {
    pub peer: Peer<RoleClient>,
    pub tools: Vec<Value>,
    pub modern: bool,
    timeout: Duration,
    service: Mutex<Option<Service>>,
    child: Mutex<Option<(Child, u32)>>,
}
impl Connection {
    pub async fn open(
        config: &Server,
        http: Option<reqwest_mcp::Client>,
        cancel: &CancellationToken,
    ) -> Result<Self> {
        ensure!(
            config.raw.get("oauth").is_none() && config.raw.get("auth").is_none(),
            "not_authenticated"
        );
        let mode = match config.raw["protocolVersion"].as_str() {
            Some("2026-07-28") => ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            },
            Some("auto") => ClientLifecycleMode::Auto {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                legacy_version: Some(ProtocolVersion::LATEST),
            },
            _ => ClientLifecycleMode::Initialize,
        };
        let mut client = ClientConfig::default();
        client.client_info.name = "zcode-cli-rust".into();
        client.client_info.version = env!("CARGO_PKG_VERSION").into();
        let lifecycle = CancellationToken::new();
        let mut owned = None;
        let init = async {
            if config.transport == "stdio" {
                let mut command = Command::new(config.raw["command"].as_str().unwrap());
                command
                    .args(super::extension_config::strings(&config.raw["args"]))
                    .current_dir(&config.cwd)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .kill_on_drop(true);
                #[cfg(unix)]
                command.process_group(0);
                if let Some(env) = config.raw["env"].as_object() {
                    command.envs(env.iter().map(|(k, v)| (k, v.as_str().unwrap())));
                }
                let mut child = command.spawn().context("process_start_failed")?;
                let pid = child.id().context("process_start_failed")?;
                let input = child.stdin.take().unwrap();
                let output = child.stdout.take().unwrap();
                owned = Some((child, pid));
                // SDK 默认读行无界；使用有界 codec，坏帧立即断开而非无限缓存或跳过。
                let reader = FramedRead::new(
                    output,
                    rmcp::transport::async_rw::JsonRpcMessageCodec::<
                        rmcp::model::ServerJsonRpcMessage,
                    >::new_with_max_length(8 * 1024 * 1024),
                )
                .take_while(|r| std::future::ready(r.is_ok()))
                .filter_map(|r| std::future::ready(r.ok()));
                let writer = FramedWrite::new(
                    input,
                    rmcp::transport::async_rw::JsonRpcMessageCodec::<
                        rmcp::model::ClientJsonRpcMessage,
                    >::new_with_max_length(8 * 1024 * 1024),
                );
                Ok::<Service, anyhow::Error>(
                    serve_client_with_lifecycle_and_ct(
                        client,
                        (writer, reader),
                        mode,
                        lifecycle.clone(),
                    )
                    .await
                    .context("protocol_negotiation_failed")?,
                )
            } else if config.transport == "sse" {
                let transport = super::mcp_sse::Transport::open(
                    config,
                    http.context("HTTP client missing")?,
                    cancel,
                )
                .await?;
                Ok(
                    serve_client_with_lifecycle_and_ct(client, transport, mode, lifecycle.clone())
                        .await
                        .context("protocol_negotiation_failed")?,
                )
            } else {
                use rmcp::transport::{
                    StreamableHttpClientTransport,
                    streamable_http_client::StreamableHttpClientTransportConfig,
                };
                let mut options = StreamableHttpClientTransportConfig::with_uri(
                    config.raw["url"].as_str().unwrap().to_owned(),
                );
                options.max_sse_event_size = 8 * 1024 * 1024;
                options.reinit_on_expired_session = false;
                options.max_concurrent_requests = 4;
                options.control_request_timeout = Duration::from_secs(2);
                if let Some(headers) = config.raw["headers"].as_object() {
                    for (name, value) in headers {
                        options.custom_headers.insert(
                            name.parse().context("config_invalid")?,
                            value.as_str().unwrap().parse().context("config_invalid")?,
                        );
                    }
                }
                let transport = StreamableHttpClientTransport::with_client(
                    http.context("HTTP client missing")?,
                    options,
                );
                Ok(
                    serve_client_with_lifecycle_and_ct(client, transport, mode, lifecycle.clone())
                        .await
                        .context("protocol_negotiation_failed")?,
                )
            }
        };
        let result = tokio::select! {biased; _=cancel.cancelled()=>Err(anyhow::anyhow!("Cancelled")), result=tokio::time::timeout(config.timeout,init)=>result.unwrap_or_else(|_|Err(anyhow::anyhow!("connection_timeout")))};
        let service = match result {
            Ok(service) => service,
            Err(error) => {
                lifecycle.cancel();
                cleanup_child(&mut owned).await?;
                return Err(error);
            }
        };
        let modern = service
            .peer_info()
            .is_some_and(|i| i.protocol_version >= ProtocolVersion::V_2026_07_28);
        let mut connection = Self {
            peer: service.peer().clone(),
            tools: vec![],
            modern,
            timeout: config.timeout,
            service: Mutex::new(Some(service)),
            child: Mutex::new(owned),
        };
        match connection.discover(cancel).await {
            Ok(tools) => connection.tools = tools,
            Err(error) => {
                connection.close().await?;
                return Err(error.context("tool_list_failed"));
            }
        }
        Ok(connection)
    }
    async fn discover(&self, cancel: &CancellationToken) -> Result<Vec<Value>> {
        let mut tools = vec![];
        let mut cursor = Value::Null;
        let mut seen = BTreeSet::new();
        for _ in 0..128 {
            let params = if cursor.is_null() {
                json!({})
            } else {
                json!({"cursor":cursor})
            };
            let result = self.request("tools/list", params, cancel).await?;
            tools.extend(
                result["tools"]
                    .as_array()
                    .context("Invalid MCP tools list")?
                    .iter()
                    .cloned(),
            );
            ensure!(tools.len() <= 10_000, "MCP tool count exceeded");
            cursor = result["nextCursor"].clone();
            if cursor.is_null() {
                return Ok(tools);
            }
            ensure!(
                cursor.is_string() && seen.insert(cursor.to_string()),
                "MCP pagination loop"
            );
        }
        bail!("MCP tool pages exceeded")
    }
    async fn request(
        &self,
        method: &str,
        params: Value,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        let request: ClientRequest =
            serde_json::from_value(json!({"method":method,"params":params}))?;
        let handle = tokio::select! {biased;_=cancel.cancelled()=>bail!("Cancelled"),result=self.peer.send_cancellable_request(request,PeerRequestOptions::with_timeout(self.timeout))=>result.context("MCP request dispatch failed")?};
        let id = handle.id.clone();
        let result = tokio::select! {biased;
            _=cancel.cancelled()=>{
                let _=tokio::time::timeout(Duration::from_secs(2),self.peer.notify_cancelled(rmcp::model::CancelledNotificationParam::new(Some(id),Some("Cancelled".into())))).await;
                Err(anyhow::anyhow!("Cancelled"))
            },
            result=handle.await_response()=>result.map_err(|_|anyhow::anyhow!("MCP request failed or timed out")),
        };
        if result.is_err() && method == "tools/call" {
            self.close().await?;
        }
        Ok(serde_json::to_value(result?)?)
    }
    pub async fn call(
        &self,
        name: &str,
        args: &Value,
        cancel: &CancellationToken,
    ) -> Result<ToolOutput> {
        let result = self
            .request("tools/call", json!({"name":name,"arguments":args}), cancel)
            .await?;
        let mut content = vec![];
        for part in result["content"].as_array().into_iter().flatten() {
            if let Some(text) = part["text"]
                .as_str()
                .or_else(|| part["resource"]["text"].as_str())
            {
                content.push(text.to_owned());
            } else {
                content.push(serde_json::to_string(part)?);
            }
        }
        if let Some(structured) = result.get("structuredContent") {
            content.push(serde_json::to_string(structured)?);
        }
        let mut content = content.join("\n");
        if content.len() > crate::domain::MAX_TOOL_BYTES {
            super::tools::truncate_utf8(&mut content, crate::domain::MAX_TOOL_BYTES);
            content.push_str("\n[MCP result truncated]");
        }
        let mut output = ToolOutput::text(content);
        output.failed = result["isError"] == true;
        Ok(output)
    }
    pub async fn close(&self) -> Result<()> {
        if let Some(mut service) = self.service.lock().await.take() {
            // 先断协议再等待进程树；不能只 drop transport 后宣告停止完成。
            let mut child = self.child.lock().await.take();
            let (protocol, process) = tokio::join!(service.close(), cleanup_child(&mut child));
            process?;
            protocol.context(ProcessCleanupFailure)?;
        }
        Ok(())
    }
}
async fn cleanup_child(child: &mut Option<(Child, u32)>) -> Result<()> {
    if let Some((mut child, pid)) = child.take()
        && let Err(error) = super::tool_process::terminate(&mut child, pid, true).await
    {
        use std::io::Write;
        let _ = writeln!(
            std::io::stderr().lock(),
            "MCP process cleanup failed: {error:#}"
        );
        return Err(error.context(ProcessCleanupFailure));
    }
    Ok(())
}
