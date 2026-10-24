//! 管理帧（docs/specs/rust-cli/remote-exec.md §19.6）：常驻执行器的状态、停止、空闲时停止。
//!
//! 不经账本、不需要客户端实例：只在一条连接的第一帧出现（代替 `hello`），服务端回一帧 `admin_reply` 后断开。老的执行器
//! 第一帧不是 `hello` 时直接断开，问的一方按“不知道”处理。

use serde::{Deserialize, Serialize};

/// 要常驻执行器做什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdminAction {
    /// 只报告状态。
    Status,
    /// 停下（断开现有连接，结束还在跑的命令）。
    Stop,
    /// 空闲时停下，忙时不停（升级时用，§19.5：判断与停止是一步）。
    StopIfIdle,
    /// 以后加的动作（老的执行器按“只报告状态”回）。
    #[serde(other)]
    Other,
}

/// `admin` 帧的头。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminRequest {
    pub action: AdminAction,
}

/// 做了什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdminOutcome {
    /// 只报告了状态。
    Status,
    /// 正在停下：回完这一帧就退出。
    Stopping,
    /// `stop_if_idle`：不空闲，没有停。
    Busy,
    #[serde(other)]
    Other,
}

/// 常驻执行器的状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutorStatus {
    /// 它说的协议主版本。
    pub protocol: u32,
    /// 它的版本。
    pub version: String,
    /// 它的实例（`welcome.server_instance`）。
    pub instance: String,
    pub pid: u32,
    /// 它自己的可执行文件（代理与升级时用它）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<String>,
    pub uptime_ms: u64,
    /// 连着的客户端实例数。
    pub clients: u64,
    /// 正在执行的调用数。
    pub calls: u64,
    /// 开着的流数（断线的客户端的流也算：它们的事件还没 ack）。
    pub streams: u64,
    /// 执行环境里还在跑的命令数。
    pub commands: u64,
    /// 执行环境里没 ack 的后台通知数。
    pub notices: u64,
    /// 空闲：上面五项都是 0（§19.5）。
    pub idle: bool,
}

/// `admin_reply` 帧的头。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminReply {
    pub outcome: AdminOutcome,
    pub status: ExecutorStatus,
}
