//! 命令被接受时的结果（`Ack::Accepted.result`，工作线 F134，审查 K4）；从 `session_api.rs` 分出（800 行上限），经
//! `session_api` 重新导出。
//!
//! 以前结果是 `serde_json::Value`：会话 6 个文件拼 JSON，前端 `result["message"].as_str().unwrap_or_default()`；两种
//! fork 的回执一个是字符串、一个是对象，形状不对时拿空 ID 去开会话。命令与回执不落库，存储不变。

use serde::{Deserialize, Serialize};

use crate::env::EnvPath;
use crate::env_rewind::RestoreAction;
use crate::ids::{MachineName, MessageId, SessionId};
use crate::manage::DirectoryOutcome;
use crate::message::Message;

/// 命令被接受时带回的结果；多数命令是 [`CommandResult::Done`]。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandResult {
    /// 没有要带回的。
    #[default]
    Done,
    /// `Fork` 与 `ForkFromInput`：分出的新会话（两种 fork 同一种回执）。
    Forked(Forked),
    /// `GenerateName`：起的名字与给用户的说明（`Session renamed to: …`，或新的改名先落地时的说明）。
    Renamed { name: String, message: String },
    /// `Rewind`：恢复的文件（只回退对话时为空）与对话里去掉的消息数。
    Rewound {
        restored: Vec<RewoundFile>,
        removed: usize,
    },
    /// `RestoreCancelled`：撤回的那条输入（前端放回输入框）与对话里去掉的消息数。装箱：消息比别的结果大得多。
    Restored {
        message: Box<Message>,
        removed: usize,
    },
    /// `StopTask`：给用户的说明。
    Stopped { message: String },
    /// `ChangeDirectory` 与 `ChangeDirectoryRecorded`：会话 actor 的判定（docs/specs/rust-cli/tui-manage.md §7.1）。
    Directory(DirectoryOutcome),
    /// `BackgroundForeground`：转到后台的前台命令数。
    Backgrounded { count: usize },
}

impl CommandResult {
    pub fn is_done(&self) -> bool {
        matches!(self, Self::Done)
    }
}

/// 分出的新会话（docs/specs/rust-cli/session-lifecycle.md §6.1、§6.5）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Forked {
    pub session: SessionId,
    /// 复制到原会话的哪一条（含）；什么都没复制时为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub through: Option<MessageId>,
    /// 复制了几条消息。
    pub copied: usize,
}

impl Forked {
    /// 回执的文字（Node 分叉对话的原文，`/fork` 显示它）；什么都没复制时为 None。
    pub fn response(&self) -> Option<String> {
        let through = self.through.as_ref()?;
        Some(format!(
            "Forked session {} from message {through}: copied {} messages.",
            self.session, self.copied
        ))
    }
}

/// 代码回退恢复的一个文件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewoundFile {
    pub path: EnvPath,
    pub action: RestoreAction,
    /// 文件所在的机器（R3，remote-exec.md §20.7.5）；主 env 不写出。
    #[serde(default, skip_serializing_if = "MachineName::is_primary")]
    pub machine: MachineName,
}
