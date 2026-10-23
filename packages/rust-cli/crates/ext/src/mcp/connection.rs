//! 一个服务器的连接与握手、清单拉取（docs/specs/rust-cli/ext.md §3.3）：工具、资源与 prompt 命令。

use std::sync::Arc;
use std::time::Duration;

use rust_cli_base::Spawner;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::mcp::{McpConnect, McpTransport};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::McpServer;
use super::rpc::{Client, RpcError};

/// MCP 协议版本（Node 配置的警告文字也引用它，F55）。
pub(crate) const PROTOCOL_VERSION: &str = "2025-06-18";
/// 服务器 instructions 与工具描述的上限。
pub(crate) const MAX_DESCRIPTION: usize = 2048;
/// 清单翻页的上限（防止服务器一直给 cursor）。
const MAX_PAGES: usize = 100;

/// 握手的结果。
pub(crate) struct Session {
    pub client: Arc<Client>,
    pub notices: super::notices::NoticeReceiver,
    pub capabilities: Value,
    pub instructions: Option<String>,
}

/// 超过 2048 字符时截断加 `… [truncated]`。
pub(crate) fn truncate_description(text: &str) -> String {
    if text.chars().count() > MAX_DESCRIPTION {
        let mut cut: String = text.chars().take(MAX_DESCRIPTION).collect();
        cut.push_str("… [truncated]");
        cut
    } else {
        text.to_owned()
    }
}

/// 连接并握手；整个过程受 `MCP_TIMEOUT` 限制。`client_version` 是入口给的本工程版本（`clientInfo`）。
pub(crate) async fn open(
    transport: &Arc<dyn McpTransport>,
    spawner: &Spawner,
    workspace: &WorkspaceRef,
    server: &McpServer,
    timeout: Duration,
    client_version: &str,
) -> Result<Session, String> {
    let timed_out = || {
        format!(
            "MCP server \"{}\" connection timed out after {}ms",
            server.name,
            timeout.as_millis()
        )
    };
    let handshake = async {
        let connection = transport
            .connect(
                workspace,
                McpConnect {
                    server: server.name.clone(),
                    transport: server.transport.clone(),
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        let roots = vec![workspace.root.as_str().to_owned()];
        let (client, notices) = Client::start(transport.clone(), connection, spawner, roots);
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"roots": {}},
            "clientInfo": {"name": rust_cli_base::names::CLIENT_NAME, "version": client_version},
        });
        let never = CancellationToken::new();
        let result = match client.request("initialize", params, timeout, &never).await {
            Ok(result) => result,
            Err(error) => {
                client.close().await;
                // 缺陷修复：initialize 自己的期限与整个握手的期限相同，总是先到，原来报的是 RPC 的
                // `timed out`；按 spec（ext.md §3.3）报连接超时的文本。
                return Err(match error {
                    RpcError::TimedOut => timed_out(),
                    other => other.to_string(),
                });
            }
        };
        if let Err(error) = client.notify("notifications/initialized", None).await {
            client.close().await;
            return Err(error.to_string());
        }
        let instructions = result
            .get("instructions")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(truncate_description);
        Ok(Session {
            client,
            notices,
            capabilities: result.get("capabilities").cloned().unwrap_or(Value::Null),
            instructions,
        })
    };
    match tokio::time::timeout(timeout, handshake).await {
        Ok(result) => result,
        Err(_) => Err(timed_out()),
    }
}

/// `tools/list` 一类的清单，按 `nextCursor` 翻页，取 `key` 下的数组。
pub(crate) async fn list(
    client: &Client,
    method: &str,
    key: &str,
    timeout: Duration,
) -> Result<Vec<Value>, RpcError> {
    let never = CancellationToken::new();
    let mut items = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let params = match &cursor {
            Some(cursor) => json!({"cursor": cursor}),
            None => json!({}),
        };
        let page = client.request(method, params, timeout, &never).await?;
        if let Some(found) = page.get(key).and_then(Value::as_array) {
            items.extend(found.iter().cloned());
        }
        cursor = page
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    Ok(items)
}

pub(crate) fn has_capability(capabilities: &Value, name: &str) -> bool {
    capabilities.get(name).is_some_and(|value| !value.is_null())
}
