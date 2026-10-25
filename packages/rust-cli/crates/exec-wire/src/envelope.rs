//! 帧头（docs/specs/rust-cli/remote-exec.md §18.2）：请求、回复、流的事件、ack、关闭流、心跳，以及线上的错误。

use serde::{Deserialize, Serialize};

use crate::dto::command::CommandEvent;
use crate::dto::mcp::McpEvent;
use crate::dto::shell::ShellNotice;
use crate::frame::Frame;
use crate::op::Op;
use crate::reply::Reply;
use crate::request::Request;

/// 线上的错误（§18.3）：前七种与 kernel-api `EnvError` 一一对应；`bad_request`、`interrupted` 是协议自己的。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WireError {
    #[error("not found: {path}")]
    NotFound { path: String },
    #[error("precondition failed: {path}")]
    PreconditionFailed { path: String },
    #[error("permission denied: {message}")]
    PermissionDenied { message: String },
    #[error("stale operation context: {message}")]
    Stale { message: String },
    #[error("operation {operation} reused with different arguments")]
    OperationConflict { operation: String },
    #[error("io error: {message}")]
    Io { message: String },
    #[error("not implemented: {what}")]
    Unsupported { what: String },
    /// 请求解不开（对方编码有误）。
    #[error("bad request: {message}")]
    BadRequest { message: String },
    /// 执行器重启（换了实例），这次调用的结果不明，不重跑（§18.5）。
    #[error("interrupted: {message}")]
    Interrupted { message: String },
}

/// 请求帧的头：`call` 是调用 ID（客户端实例内单调递增，重连不变）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestHeader {
    pub call: u64,
    pub request: Request,
    /// 补发一个流时：客户端已经收到的最后一项的 `seq`（§18.5）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_after: Option<u64>,
}

/// 请求头解不开时能认出的部分（服务端据此回错误，连接照常）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadRequest {
    pub call: Option<u64>,
    pub error: WireError,
}

impl RequestHeader {
    /// 解请求帧的头。解不开时尽量认出调用 ID：`op` 不认识（新客户端的新消息）是 `unsupported`，其余是
    /// `bad_request`。
    pub fn parse(frame: &Frame) -> Result<Self, BadRequest> {
        let loose: serde_json::Value =
            serde_json::from_slice(&frame.header).map_err(|error| BadRequest {
                call: None,
                error: WireError::BadRequest {
                    message: error.to_string(),
                },
            })?;
        let call = loose.get("call").and_then(serde_json::Value::as_u64);
        serde_json::from_value(loose.clone()).map_err(|error| {
            let op = loose
                .pointer("/request/op")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let error = if Op::from_name(op).is_none() {
                WireError::Unsupported {
                    what: format!("message {op}"),
                }
            } else {
                WireError::BadRequest {
                    message: error.to_string(),
                }
            };
            BadRequest { call, error }
        })
    }
}

/// 回复帧的头。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyHeader {
    pub call: u64,
    pub result: ReplyResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyResult {
    Ok(Reply),
    Err(WireError),
}

/// 流的一项（事件帧的头）。`seq` 在这个调用里从 1 起递增。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventHeader {
    pub call: u64,
    pub seq: u64,
    pub item: StreamItem,
}

/// 流里的一项：端口给的事件、流里的错误，或结束（最后一项）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StreamItem {
    Command {
        event: CommandEvent,
    },
    Notice {
        notice: ShellNotice,
    },
    /// MCP 连接上的一个事件（R5，remote-exec.md §22.4）。
    Mcp {
        event: McpEvent,
    },
    Error {
        error: WireError,
    },
    End,
}

/// 客户端按批 ack（§18.5）：收到的回复，与每个流已经消费到的 `seq`（累计）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ack {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replies: Vec<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub streams: Vec<StreamAck>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamAck {
    pub call: u64,
    pub seq: u64,
}

/// 客户端不再要这个流。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Close {
    pub call: u64,
}

/// 心跳（`ping` 与 `pong` 共用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Beat {
    pub nonce: u64,
}
