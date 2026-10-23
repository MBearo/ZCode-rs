//! MCP 传输的本地实现（docs/specs/rust-cli/ext.md §3.2）：kernel-api `McpTransport` 端口。
//!
//! - stdio：经 `rust-cli-proc` 的账本启动服务器进程，一行一条 JSON-RPC 消息；
//! - streamable HTTP 与 SSE：经 `rust-cli-net` 的 [`Egress`]。
//!
//! 每条连接一个由句柄拥有的任务（`rust_cli_base::owned`），任务拥有进程或 HTTP 流；连接从表里移除
//! 或传输被释放时任务随之中止（进程树随 `Tracked` 释放而结束）。
//!
//! 通道都有界（docs/specs/rust-cli/ext.md §3.2“通道与溢出”，审查 R1-18）：服务器发来的事件最多排
//! [`EVENTS_CAPACITY`] 条，满了时生产方等待（stdio 停读 stdout、HTTP 停读响应流，背压到服务器，响应一条不丢）；
//! 发往 stdio 服务器的消息最多排 [`OUTBOX_CAPACITY`] 条，满了说明服务器不读输入，发送立即报错（调用方不被卡住）。

mod http;
pub(crate) mod stdio;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rust_cli_base::secret::EnvSnapshot;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::mcp::{
    McpConnect, McpConnectionId, McpError, McpTransport, McpTransportConfig, McpTransportEvent,
};
use rust_cli_net::Egress;
use rust_cli_proc::ledger::Ledger;
use serde_json::Value;
use tokio::sync::mpsc;

/// 服务器发来、还没被读走的事件最多这么多条（满了生产方等待）。
pub(crate) const EVENTS_CAPACITY: usize = 256;
/// 发往 stdio 服务器、还没写进 stdin 的消息最多这么多条（满了发送报错）。
pub(crate) const OUTBOX_CAPACITY: usize = 256;

/// 服务器 → 读者的事件通道的发送端。
pub(crate) type EventSender = mpsc::Sender<McpTransportEvent>;

/// 一条连接：发消息的入口、事件的接收端（取走一次），以及拥有进程或流的任务。
pub(crate) struct Connection {
    outbox: Outbox,
    events: Option<mpsc::Receiver<McpTransportEvent>>,
    task: rust_cli_base::OwnedTask<()>,
}

/// 发消息：stdio 交给连接任务写 stdin；HTTP 由连接自己发请求。
pub(crate) enum Outbox {
    Stdio(Arc<stdio::StdioLink>),
    Http(Arc<http::HttpLink>),
}

/// 本机的 MCP 传输。
pub struct LocalMcpTransport {
    ledger: Ledger,
    egress: Option<Arc<Egress>>,
    /// 入口捕获的进程环境：stdio 服务器的环境按白名单投影（与 Bash 相同）。
    env: EnvSnapshot,
    connections: Mutex<HashMap<String, Connection>>,
    next: AtomicU64,
}

impl LocalMcpTransport {
    /// `ledger` 是进程树账本目录（与 Bash 相同）；没有 `egress` 时 HTTP / SSE 服务器连不上。
    pub fn new(ledger: PathBuf, env: EnvSnapshot, egress: Option<Arc<Egress>>) -> Self {
        Self {
            ledger: Ledger::open(ledger),
            egress,
            env,
            connections: Mutex::default(),
            next: AtomicU64::new(0),
        }
    }

    fn connections(&self) -> MutexGuard<'_, HashMap<String, Connection>> {
        self.connections
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn egress(&self) -> Result<Arc<Egress>, McpError> {
        self.egress.clone().ok_or_else(|| McpError::Unsupported {
            what: "MCP over HTTP without network egress".to_owned(),
        })
    }
}

#[async_trait]
impl McpTransport for LocalMcpTransport {
    async fn connect(
        &self,
        workspace: &WorkspaceRef,
        request: McpConnect,
    ) -> Result<McpConnectionId, McpError> {
        let (sender, events) = mpsc::channel(EVENTS_CAPACITY);
        let (outbox, task) = match &request.transport {
            McpTransportConfig::Stdio {
                command,
                args,
                env,
                cwd,
            } => {
                // Node `resolve(workingDirectory, cwd)`：相对路径相对工作区根，绝对路径原样（ext.md §3.2）。
                let root = std::path::Path::new(workspace.root.as_str());
                let cwd = cwd
                    .as_deref()
                    .map_or_else(|| root.to_path_buf(), |cwd| root.join(cwd));
                let launch = stdio::Launch {
                    server: &request.server,
                    command,
                    args,
                    env,
                    cwd: &cwd.to_string_lossy(),
                    snapshot: &self.env,
                };
                let (outbox, task) = stdio::start(&self.ledger, launch, sender).await?;
                (Outbox::Stdio(outbox), task)
            }
            McpTransportConfig::Http { url, headers } => {
                let (link, task) = http::streamable(self.egress()?, url, headers, sender);
                (Outbox::Http(link), task)
            }
            McpTransportConfig::Sse { url, headers } => {
                let (link, task) = http::sse(self.egress()?, url, headers, sender).await?;
                (Outbox::Http(link), task)
            }
        };
        let id = format!("mcp-{}", self.next.fetch_add(1, Ordering::Relaxed));
        self.connections().insert(
            id.clone(),
            Connection {
                outbox,
                events: Some(events),
                task,
            },
        );
        Ok(McpConnectionId::new(id))
    }

    async fn send(&self, connection: &McpConnectionId, message: Value) -> Result<(), McpError> {
        let outbox = match self.connections().get(connection.as_str()) {
            Some(Connection {
                outbox: Outbox::Stdio(link),
                ..
            }) => Outbox::Stdio(link.clone()),
            Some(Connection {
                outbox: Outbox::Http(link),
                ..
            }) => Outbox::Http(link.clone()),
            None => {
                return Err(McpError::Closed {
                    message: format!("unknown connection {connection:?}"),
                });
            }
        };
        match outbox {
            Outbox::Stdio(link) => link.send(message),
            Outbox::Http(link) => link.send(message).await,
        }
    }

    fn events(&self, connection: &McpConnectionId) -> BoxStream<'static, McpTransportEvent> {
        let events = self
            .connections()
            .get_mut(connection.as_str())
            .and_then(|found| found.events.take());
        match events {
            Some(events) => tokio_stream_from(events),
            None => futures_util::stream::empty().boxed(),
        }
    }

    async fn close(&self, connection: &McpConnectionId) -> Result<(), McpError> {
        let Some(found) = self.connections().remove(connection.as_str()) else {
            return Ok(());
        };
        // 连接任务收到关闭后结束进程树（或停止 HTTP 流）并送出 Closed，然后结束；等它结束。关闭不经发件箱：
        // 发件箱满（服务器不读输入）时也能关。
        match &found.outbox {
            Outbox::Stdio(link) => link.close(),
            Outbox::Http(link) => link.close().await,
        }
        let _ = found.task.await;
        Ok(())
    }
}

/// 事件的接收端换成流（`events` 只能取一次）。
fn tokio_stream_from(
    mut events: mpsc::Receiver<McpTransportEvent>,
) -> BoxStream<'static, McpTransportEvent> {
    futures_util::stream::poll_fn(move |context| events.poll_recv(context)).boxed()
}

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "bounds_tests.rs"]
mod bounds_tests;
