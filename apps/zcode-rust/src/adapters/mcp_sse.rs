use super::mcp_config::Server;
use anyhow::{Context, Result, ensure};
use futures_util::StreamExt;
use rmcp::{
    RoleClient,
    model::{ClientJsonRpcMessage, ServerJsonRpcMessage},
};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::{CancellationToken, DropGuard};

pub(super) struct Transport {
    client: reqwest_mcp::Client,
    endpoint: Arc<str>,
    headers: reqwest_mcp::header::HeaderMap,
    rx: mpsc::Receiver<ServerJsonRpcMessage>,
    worker: Option<tokio::task::JoinHandle<()>>,
    cancel: CancellationToken,
    _guard: DropGuard,
}
impl Transport {
    pub async fn open(
        config: &Server,
        client: reqwest_mcp::Client,
        stop: &CancellationToken,
    ) -> Result<Self> {
        let url = url::Url::parse(config.raw["url"].as_str().unwrap())?;
        let mut headers = reqwest_mcp::header::HeaderMap::new();
        if let Some(values) = config.raw["headers"].as_object() {
            for (k, v) in values {
                headers.insert(
                    k.parse::<reqwest_mcp::header::HeaderName>()?,
                    v.as_str().unwrap().parse()?,
                );
            }
        }
        let response = tokio::select! {biased;_=stop.cancelled()=>anyhow::bail!("Cancelled"),response=client.get(url.clone()).headers(headers.clone()).header("Accept","text/event-stream").send()=>response.context("network_unreachable")?.error_for_status().context("network_unreachable")?};
        let (tx, rx) = mpsc::channel(32);
        let (endpoint, ready) = oneshot::channel();
        let cancel = CancellationToken::new();
        let worker_cancel = cancel.clone();
        let guard = cancel.clone().drop_guard();
        let worker = tokio::spawn(async move {
            let stream = async {
                let mut stream = response.bytes_stream();
                let mut decoder = Decoder::default();
                let mut endpoint = Some(endpoint);
                while let Some(bytes) = stream.next().await {
                    for (kind, data) in decoder.push(&bytes?)? {
                        if kind == "endpoint" {
                            if let Some(endpoint) = endpoint.take() {
                                let parsed = url.join(&data)?;
                                ensure!(
                                    parsed.origin() == url.origin(),
                                    "SSE endpoint changed origin"
                                );
                                let _ = endpoint.send(parsed.to_string());
                            }
                        } else if kind == "message" || kind.is_empty() {
                            tx.send(serde_json::from_str::<ServerJsonRpcMessage>(&data)?)
                                .await?;
                        }
                    }
                }
                Ok::<(), anyhow::Error>(())
            };
            tokio::select! {biased;_=worker_cancel.cancelled()=>(),_=stream=>()}
        });
        let endpoint = tokio::select! {biased;_=stop.cancelled()=>anyhow::bail!("Cancelled"), result=ready=>result.context("protocol_negotiation_failed")?};
        Ok(Self {
            client,
            endpoint: endpoint.into(),
            headers,
            rx,
            worker: Some(worker),
            cancel,
            _guard: guard,
        })
    }
}
impl rmcp::transport::Transport<RoleClient> for Transport {
    type Error = std::io::Error;
    fn send(
        &mut self,
        item: ClientJsonRpcMessage,
    ) -> impl std::future::Future<Output = std::io::Result<()>> + Send + 'static {
        let client = self.client.clone();
        let endpoint = self.endpoint.clone();
        let headers = self.headers.clone();
        let cancel = self.cancel.clone();
        async move {
            tokio::select! {biased;_=cancel.cancelled()=>Err(std::io::Error::other("MCP SSE closed")),result=client.post(endpoint.as_ref()).headers(headers).json(&item).send()=>{
                result.and_then(reqwest_mcp::Response::error_for_status).map(|_|()).map_err(|_|std::io::Error::other("MCP SSE post failed"))
            }}
        }
    }
    async fn receive(&mut self) -> Option<ServerJsonRpcMessage> {
        self.rx.recv().await
    }
    async fn close(&mut self) -> std::io::Result<()> {
        self.cancel.cancel();
        if let Some(worker) = self.worker.take() {
            worker.await.map_err(std::io::Error::other)?;
        }
        Ok(())
    }
}
#[derive(Default)]
struct Decoder {
    line: Vec<u8>,
    kind: String,
    data: String,
}
impl Decoder {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<(String, String)>> {
        let mut events = vec![];
        for part in bytes.split_inclusive(|b| *b == b'\n') {
            ensure!(
                self.line.len() + part.len() <= 8 * 1024 * 1024,
                "MCP SSE line too large"
            );
            self.line.extend_from_slice(part);
            if part.last() != Some(&b'\n') {
                continue;
            }
            let line = std::str::from_utf8(&self.line)?.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if !self.data.is_empty() {
                    events.push((
                        std::mem::take(&mut self.kind),
                        std::mem::take(&mut self.data),
                    ));
                }
            } else if let Some(data) = line.strip_prefix("data:") {
                if !self.data.is_empty() {
                    self.data.push('\n');
                }
                self.data.push_str(data.strip_prefix(' ').unwrap_or(data));
            } else if let Some(kind) = line.strip_prefix("event:") {
                self.kind = kind.trim().into();
            }
            ensure!(
                self.data.len() <= 8 * 1024 * 1024,
                "MCP SSE event too large"
            );
            self.line.clear();
        }
        Ok(events)
    }
}
