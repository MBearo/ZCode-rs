//! 远端机器上的 MCP 管道（docs/specs/rust-cli/remote-exec.md §22.4、§22.6）：kernel-api `McpTransport` 的每个方法是一条
//! `mcp.*` 消息。服务器的配置原样发出（`${VAR}` 由执行器用它自己的环境展开，§22.7），JSON-RPC 消息原样往返。
//!
//! 事件流（`mcp.events`）里的错误（包括执行器重启后的 `interrupted`）与连接层的错误（机器断开、重连次数用完）都换成
//! 一项 `Closed{reason}` 再结束：ext 照服务器退出处理（ext.md §3.3.3）。

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::{self, BoxStream};
use rust_cli_exec_client::ClientError;
use rust_cli_exec_wire::BodyBuilder;
use rust_cli_exec_wire::envelope::{ReplyResult, StreamItem, WireError};
use rust_cli_exec_wire::reply::Reply;
use rust_cli_exec_wire::request::Request;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::mcp::{
    McpConnect, McpConnectionId, McpError, McpTransport, McpTransportEvent,
};

use crate::{RemoteEnvironment, from_wire, to_wire};

/// 连接层的错误：执行器没报告 `mcp` 是 `Unsupported`，其余（断开、次数用完）是连接关了。
fn client_error(error: ClientError) -> McpError {
    match error {
        ClientError::Unsupported { what } => McpError::Unsupported { what },
        other => McpError::Closed {
            message: other.to_string(),
        },
    }
}

impl RemoteEnvironment {
    async fn mcp_call(&self, request: Request) -> Result<Reply, Result<WireError, ClientError>> {
        let op = request.op();
        let answer = self
            .client()
            .call(request, BodyBuilder::new().finish())
            .await
            .map_err(Err)?;
        match answer.result {
            ReplyResult::Ok(reply) if reply.op() == op => Ok(reply),
            ReplyResult::Ok(reply) => Err(Ok(WireError::Io {
                message: crate::mismatch(op.as_str(), reply.op().as_str()).to_string(),
            })),
            ReplyResult::Err(wire) => Err(Ok(wire)),
        }
    }

    /// 期望集合（§22.5）：这台机器上应该有的全部服务器（配置的原文）；执行器结束不在里面的，返回它们的名字。执行器
    /// 没报告 `mcp` 时什么都不做。
    pub async fn declare_mcp(
        &self,
        workspace: &WorkspaceRef,
        servers: Vec<McpConnect>,
    ) -> Result<Vec<String>, McpError> {
        let request = Request::McpDeclare {
            workspace: to_wire::workspace(workspace),
            servers: servers.into_iter().map(to_wire::mcp_declared).collect(),
        };
        match self.mcp_call(request).await {
            Ok(Reply::McpDeclare { closed }) => Ok(closed),
            Ok(_) => Ok(Vec::new()),
            Err(Err(ClientError::Unsupported { .. })) => Ok(Vec::new()),
            Err(Err(error)) => Err(client_error(error)),
            Err(Ok(wire)) => Err(from_wire::mcp_error(wire)),
        }
    }
}

#[async_trait]
impl McpTransport for RemoteEnvironment {
    async fn connect(
        &self,
        workspace: &WorkspaceRef,
        request: McpConnect,
    ) -> Result<McpConnectionId, McpError> {
        let McpConnect { server, transport } = request;
        let request = Request::McpConnect {
            workspace: to_wire::workspace(workspace),
            server,
            transport: to_wire::mcp_transport(transport),
        };
        match self.mcp_call(request).await {
            Ok(Reply::McpConnect { connection }) => Ok(McpConnectionId::new(connection)),
            Ok(other) => Err(McpError::Connect {
                message: crate::mismatch("mcp.connect", other.op().as_str()).to_string(),
            }),
            Err(Err(ClientError::Unsupported { what })) => Err(McpError::Unsupported { what }),
            // 起不来、连不上：执行器的原文（例如 `MCP server "x" failed to start: …`）。
            Err(Err(error)) => Err(McpError::Connect {
                message: error.to_string(),
            }),
            Err(Ok(wire)) => Err(McpError::Connect {
                message: from_wire::mcp_text(&wire),
            }),
        }
    }

    async fn send(
        &self,
        connection: &McpConnectionId,
        message: serde_json::Value,
    ) -> Result<(), McpError> {
        let request = Request::McpSend {
            connection: connection.as_str().to_owned(),
            message,
        };
        match self.mcp_call(request).await {
            Ok(_) => Ok(()),
            Err(Err(error)) => Err(client_error(error)),
            Err(Ok(wire)) => Err(from_wire::mcp_error(wire)),
        }
    }

    fn events(&self, connection: &McpConnectionId) -> BoxStream<'static, McpTransportEvent> {
        let client = self.client().clone();
        let request = Request::McpEvents {
            connection: connection.as_str().to_owned(),
        };
        let closed = |reason: String| McpTransportEvent::Closed {
            reason: Some(reason),
            session_expired: false,
        };
        stream::once(async move { client.stream(request, BodyBuilder::new().finish()).await })
            .flat_map(move |opened| match opened {
                Ok(items) => items
                    .map(move |item| match item {
                        Ok(delivered) => match delivered.item {
                            StreamItem::Mcp { event } => from_wire::mcp_event(event),
                            StreamItem::Error { error } => closed(from_wire::mcp_text(&error)),
                            other => closed(format!("unexpected MCP stream item {other:?}")),
                        },
                        Err(error) => closed(error.to_string()),
                    })
                    .boxed(),
                Err(error) => stream::iter([closed(error.to_string())]).boxed(),
            })
            // 第一项 Closed 之后结束（同 kernel-api 的约定：Closed 之后不再有事件）。
            .scan(false, |done, item| {
                let next = (!*done).then(|| {
                    *done = matches!(item, McpTransportEvent::Closed { .. });
                    item
                });
                futures_util::future::ready(next)
            })
            .boxed()
    }

    async fn close(&self, connection: &McpConnectionId) -> Result<(), McpError> {
        let request = Request::McpClose {
            connection: connection.as_str().to_owned(),
        };
        match self.mcp_call(request).await {
            Ok(_) => Ok(()),
            Err(Err(error)) => Err(client_error(error)),
            Err(Ok(wire)) => Err(from_wire::mcp_error(wire)),
        }
    }
}
