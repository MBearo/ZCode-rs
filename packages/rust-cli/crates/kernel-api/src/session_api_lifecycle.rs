//! 会话生命周期的类型（docs/specs/rust-cli/session-lifecycle.md）；从 `session_api.rs` 拆出（400 行上限），
//! 经 `session_api` 重新导出，路径不变。

use serde::{Deserialize, Serialize};

use super::FileChange;
use crate::ids::MessageId;

/// SessionStart hook 的 `source`（docs/specs/rust-cli/session-lifecycle.md §3.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartSource {
    Startup,
    Resume,
    Clear,
    Compact,
    Fork,
}

/// SessionEnd hook 的 `reason`（§3.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    Clear,
    Resume,
    Logout,
    PromptInputExit,
    Other,
}

/// rewind 选择器里的一条 user 消息（§5.1）：这一回合改过的文件与能否恢复代码。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewindOption {
    pub message: MessageId,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<FileChange>,
    /// 这一回合起有可以恢复的 checkpoint。
    #[serde(default)]
    pub code: bool,
}
