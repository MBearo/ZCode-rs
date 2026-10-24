//! 远端执行器的客户端会话层（docs/specs/rust-cli/remote-exec.md §15.3、§18.4、§18.5）：一条连接上的握手、请求与回复的
//! 关联、调用 ID、流的 `seq` 与批量 ack、断线后重连与补发、心跳、背压与大块分片。
//!
//! 只认识 `exec-wire` 的消息与 `exec-transport` 的 `Connector`，不认识 kernel-api：把端口方法换成请求是
//! `env-remote` 的事。
//!
//! 任务（每条连接一个读、一个写、一个心跳，外加一个管连接的监督任务）都在客户端自己的 `TaskScope` 里；最后一个
//! [`ExecClient`] 被丢掉时全部结束，传输随之关闭（stdio 的执行器读到 stdin 结束后自己退出）。

mod client;
mod connection;
mod state;
mod stream;

use std::time::Duration;

use rust_cli_base::secret::RedactedString;
use rust_cli_exec_wire::Body;
use rust_cli_exec_wire::envelope::{ReplyResult, StreamItem, WireError};
use rust_cli_exec_wire::frame::FrameError;
use rust_cli_exec_wire::handshake::RefuseCode;

pub use client::ExecClient;
pub use connection::backoff;
pub use stream::ClientStream;

/// 客户端的参数（由组装根给）。
#[derive(Debug, Clone)]
pub struct ClientOptions {
    pub client_version: String,
    /// 客户端实例：操作按（实例，调用 ID）记账，重连时不变。
    pub client_instance: String,
    pub credentials: Option<RedactedString>,
    /// 心跳间隔：每隔这么久发一个 `ping`；三个间隔没收到任何帧就当连接断了。
    pub heartbeat: Duration,
    /// 断线后重连几次、每次之间等多久（0 次：断线就是这台机器断开；remote-exec.md §19.8）。
    pub reconnect: Reconnect,
    /// 大消息拆片的大小。
    pub part_size: usize,
}

/// 重连的次数与退避（§19.8）：第一次等 `delay`，之后每次翻倍，最多等 `max_delay`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reconnect {
    pub attempts: u32,
    pub delay: Duration,
    pub max_delay: Duration,
}

/// 连接的状态（§19.8）：machines 的状态机订阅它。只由管连接的任务改。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    /// 握手完成：第几次连接、执行器的实例。
    Connected { epoch: u64, server_instance: String },
    /// 连接断了，正在重连：第几次尝试（0：刚断，还没开始等）、断开的原因。
    Reconnecting { attempt: u32, reason: String },
    /// 不再工作（关闭、重连次数用完、被拒绝、协议不对）。
    Closed { error: ClientError },
}

impl ClientOptions {
    pub fn new(client_version: impl Into<String>, client_instance: impl Into<String>) -> Self {
        Self {
            client_version: client_version.into(),
            client_instance: client_instance.into(),
            credentials: None,
            heartbeat: Duration::from_secs(10),
            reconnect: Reconnect {
                attempts: 0,
                delay: Duration::from_millis(500),
                max_delay: Duration::from_secs(15),
            },
            part_size: rust_cli_exec_wire::frame::PART_SIZE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClientError {
    /// 连不上（第一次连接或重连）。
    #[error("cannot reach the executor: {0}")]
    Connect(String),
    /// 执行器拒绝握手。
    #[error("the executor refused the connection ({code:?}): {message}")]
    Refused { code: RefuseCode, message: String },
    /// 对方违反协议（主版本不同、回复对不上、帧解不开）。
    #[error("executor protocol error: {0}")]
    Protocol(String),
    /// 连接断了、不再重连；还没结束的调用都以它结束。
    #[error("the executor connection was lost: {0}")]
    Disconnected(String),
    /// 执行器换了实例：有副作用的调用结果不明，不重跑（remote-exec.md §18.5）。
    #[error("the executor restarted before {op} finished; its outcome is unknown")]
    Interrupted { op: String },
    /// 执行器没有报告这个端口的能力位。
    #[error("not implemented by the executor: {what}")]
    Unsupported { what: String },
    /// 消息超过上限。
    #[error("{0}")]
    TooLarge(String),
    /// 客户端已经关闭。
    #[error("the executor client is closed")]
    Closed,
}

impl From<FrameError> for ClientError {
    fn from(error: FrameError) -> Self {
        match error {
            FrameError::TooLarge { .. } => Self::TooLarge(error.to_string()),
            other => Self::Protocol(other.to_string()),
        }
    }
}

/// 一次调用的回复：结果（值或线上的错误）与二进制段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub result: ReplyResult,
    pub body: Body,
}

impl Answer {
    /// 线上的错误（不是值）时取出来。
    pub fn error(&self) -> Option<&WireError> {
        match &self.result {
            ReplyResult::Err(error) => Some(error),
            ReplyResult::Ok(_) => None,
        }
    }
}

/// 流的一项与它的二进制段。`end` 不交给调用方：流在它之后结束。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamEvent {
    pub item: StreamItem,
    pub body: Body,
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod client_tests;
#[cfg(test)]
#[path = "reconnect_tests.rs"]
mod reconnect_tests;
#[cfg(test)]
mod test_support;
