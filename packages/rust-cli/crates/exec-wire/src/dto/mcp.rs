//! MCP 管道的线上类型（kernel-api `mcp`，docs/specs/rust-cli/remote-exec.md §22.4）。
//!
//! 服务器的配置是原文：`${VAR}` 没展开，由执行器用它自己的环境展开（§22.7）。JSON-RPC 消息是原样的 JSON。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 一个服务器的传输配置（kernel-api `McpTransportConfig`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransport {
    Stdio {
        command: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
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

/// 连接上的事件（kernel-api `McpTransportEvent`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpEvent {
    Message {
        message: Value,
    },
    Closed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default)]
        session_expired: bool,
    },
}

/// 期望集合里的一个服务器（`mcp.declare`，remote-exec.md §22.5）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpDeclared {
    pub server: String,
    pub transport: McpTransport,
}
