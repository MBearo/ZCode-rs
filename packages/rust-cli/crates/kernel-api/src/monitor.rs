//! Monitor 交给会话 actor 的登记（工作线 F83 新增，docs/specs/rust-cli/tools-more.md §19.6）。
//!
//! Monitor 工具起好后台命令（或 WebSocket）之后，经 [`crate::orchestration::Interactions::arm_monitor`] 把这条登记
//! 交给会话 actor；切行、批、限流、到期与终态通知的状态都在 actor 里，按游标从
//! [`crate::machine::Commands::watch_command`] 读输出。

use serde::{Deserialize, Serialize};

use crate::env::EnvPath;
use crate::ids::{OperationId, ShellTaskId, ToolUseId};

/// 事件的来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MonitorSource {
    /// shell 命令：stdout 的每行是事件。
    Command,
    /// WebSocket：每个文本帧是事件，生命周期在 stderr 一侧（[`crate::env_shell::SocketEvent`]）。
    Socket,
}

/// 一个已经起好的 monitor。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorArmed {
    /// 后台命令的操作（`watch_command`、`stop_background` 用它）。
    pub operation: OperationId,
    pub task: ShellTaskId,
    pub tool_use: ToolUseId,
    /// 通知里的说明（输入的 `description`）。
    pub description: String,
    /// 到期（毫秒，已按上限截过）。
    pub timeout_ms: u64,
    pub source: MonitorSource,
    /// 输出文件（终态通知的 `<output-file>`）。
    pub output: EnvPath,
    /// 命令在哪台远端机器上（R3，remote-exec.md §20.7.2）：输出流与停止都发给它；本机为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
}
