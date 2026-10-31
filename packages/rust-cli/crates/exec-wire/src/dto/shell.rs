//! shell 状态、后台命令与终态通知的线上类型（kernel-api `env_shell`）。

use serde::{Deserialize, Serialize};

use crate::dto::command::BackgroundKind;
use crate::dto::core::Outcome;

/// 会话的 shell 状态（kernel-api `ShellState`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellState {
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outputs: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub background: Vec<String>,
}

/// 后台命令的终态通知（kernel-api `ShellNotice`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellNotice {
    pub seq: u64,
    pub operation: String,
    pub task: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use: Option<String>,
    pub description: String,
    pub output: String,
    pub outcome: Outcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub kind: BackgroundKind,
    #[serde(default)]
    pub output_bytes: u64,
}

/// 会话里的一条后台命令（kernel-api `BackgroundTask`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundTask {
    pub session: String,
    pub task: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundStatus {
    Running,
    Completed,
    Failed,
    Killed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundInfo {
    pub command: String,
    pub status: BackgroundStatus,
    /// 修复线 F117 新增的可选字段：旧执行器不写，按 Bash（docs/specs/rust-cli/remote-exec.md §18.4）。
    #[serde(default, skip_serializing_if = "BackgroundKind::is_bash")]
    pub kind: BackgroundKind,
    /// 修复线 F117 新增的可选字段：启动时的说明；旧执行器不写，按空。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BackgroundStop {
    Stopped { command: String },
    NotRunning { status: BackgroundStatus },
    NotFound,
}
