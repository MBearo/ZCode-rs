//! MCP 的传输端口与跨端口类型（docs/specs/rust-cli/ext.md §3.2、§3.4）。
//!
//! MCP 传输属于执行环境（README §6）：内核（ext）只发收 JSON-RPC 消息，协议语义（握手、工具清单、
//! 调用、通知）在内核里；本地实现在 env-local（stdio 经 proc，SSE / streamable HTTP 经 net），上云后
//! 换成远端执行环境的实现。

use std::collections::BTreeMap;

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::env::WorkspaceRef;

/// 一个服务器的传输配置（stdio / sse / http 三种）。
/// `${VAR}` 已由入口层展开。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransportConfig {
    Stdio {
        command: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        /// 加在执行环境白名单变量之上的变量。
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
        /// 进程的工作目录（Node 配置的 `cwd`）：相对路径相对工作区根，绝对路径原样；None 是工作区根
        /// （docs/specs/rust-cli/ext.md §3.1、§3.2）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },
    Sse {
        url: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
    },
    Http {
        url: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
    },
}

/// 建立一条连接。`server` 只用于日志与错误文本。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpConnect {
    pub server: String,
    pub transport: McpTransportConfig,
}

/// 执行侧给出的连接 ID。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct McpConnectionId(String);

impl McpConnectionId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 连接上的事件：收到的一条 JSON-RPC 消息，或连接已关闭（之后不再有事件）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransportEvent {
    Message {
        message: Value,
    },
    Closed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// HTTP 传输上服务器说会话已过期（404 且带会话 ID）：重连后可以重试。
        #[serde(default)]
        session_expired: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum McpError {
    /// 进程起不来、连不上、HTTP 错误状态等。
    #[error("{message}")]
    Connect { message: String },
    /// 连接已关闭或不存在。
    #[error("connection closed: {message}")]
    Closed { message: String },
    #[error("{message}")]
    Io { message: String },
    #[error("not implemented: {what}")]
    Unsupported { what: String },
}

/// MCP 传输。实现方保证：一条连接的事件按到达顺序送出；`events` 只能取一次（取走缓冲的事件与之后的
/// 事件），再取得到空流；`close` 之后送出 `Closed` 并结束事件流。
#[async_trait]
pub trait McpTransport: Send + Sync {
    async fn connect(
        &self,
        workspace: &WorkspaceRef,
        request: McpConnect,
    ) -> Result<McpConnectionId, McpError>;

    /// 发一条 JSON-RPC 消息（请求、通知或对服务器请求的响应）。
    async fn send(&self, connection: &McpConnectionId, message: Value) -> Result<(), McpError>;

    fn events(&self, connection: &McpConnectionId) -> BoxStream<'static, McpTransportEvent>;

    async fn close(&self, connection: &McpConnectionId) -> Result<(), McpError>;
}

/// 没有 MCP 传输时的空实现：连接一律失败。
pub struct NoMcpTransport;

fn unsupported() -> McpError {
    McpError::Unsupported {
        what: "MCP transport".to_owned(),
    }
}

#[async_trait]
impl McpTransport for NoMcpTransport {
    async fn connect(
        &self,
        _workspace: &WorkspaceRef,
        _request: McpConnect,
    ) -> Result<McpConnectionId, McpError> {
        Err(unsupported())
    }

    async fn send(&self, _connection: &McpConnectionId, _message: Value) -> Result<(), McpError> {
        Err(unsupported())
    }

    fn events(&self, _connection: &McpConnectionId) -> BoxStream<'static, McpTransportEvent> {
        futures_util::stream::empty().boxed()
    }

    async fn close(&self, _connection: &McpConnectionId) -> Result<(), McpError> {
        Ok(())
    }
}

/// MCP 工具的来源（服务器信息与 `alwaysLoad`）：延迟加载与规则匹配用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpToolInfo {
    /// 配置里的服务器名（未规范化）。
    pub server: String,
    /// 服务器给出的工具原名。
    pub tool: String,
    /// 服务器配置的 `alwaysLoad` 或工具的 `_meta["zcode/alwaysLoad"]` 为 true：不延迟加载。
    #[serde(default)]
    pub always_load: bool,
}

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;
