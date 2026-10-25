//! 文件 checkpoint 与工具结果的配对规则（docs/specs/rust-cli/session-lifecycle.md §4）。
//!
//! 写文件的工具在写入后把 artifact 引用放进 `ToolOutput.display.checkpoint`
//! （`{artifact, path, additions, deletions}`）。主循环先发 `ToolFinished`、再提交工具结果；提交方
//! （会话 actor、子代理的 sink）在 `ToolFinished` 时记下，提交这条工具结果时把
//! [`Fact::CheckpointCreated`](crate::store::Fact::CheckpointCreated) 放进同一个事务。两处用同一份规则，
//! 主会话与子会话的 checkpoint 形状相同。

use std::collections::HashMap;

use serde_json::Value;

use crate::clock::Ids;
use crate::ids::{MessageId, ToolUseId};
use crate::message::{ContentBlock, Message, UserOrigin};
use crate::session_api::FileChange;
use crate::store::{ArtifactRef, WorkspaceCheckpoint};
use crate::tool::ToolOutput;

/// `ToolOutput.display` 里放 checkpoint 的键。
pub const DISPLAY_KEY: &str = "checkpoint";

/// 已结束、结果还没提交的工具调用带出的 checkpoint。
#[derive(Debug, Default)]
pub struct PendingCheckpoints(HashMap<ToolUseId, Value>);

impl PendingCheckpoints {
    /// 工具结束：输出带 checkpoint 时记下。只有写入真的发生了才有 checkpoint；结果被中断文本替换时
    /// 主循环仍把它带在输出里（query.md 的执行器），所以这里不看 `is_error`。
    pub fn note(&mut self, tool_use: &ToolUseId, output: &ToolOutput) {
        let checkpoint = &output.display[DISPLAY_KEY];
        if checkpoint.is_object() {
            self.0.insert(tool_use.clone(), checkpoint.clone());
        }
    }

    /// 这条工具结果消息带出的 checkpoint 与改动摘要。`prompt` 是这次运行开头的 user 消息（Node
    /// `messageId`）；不是工具结果的消息没有 checkpoint。
    pub fn settle(
        &mut self,
        message: &Message,
        prompt: &MessageId,
        ids: &dyn Ids,
    ) -> Vec<(WorkspaceCheckpoint, FileChange)> {
        let Message::User(user) = message else {
            return Vec::new();
        };
        let UserOrigin::ToolResult { assistant } = &user.origin else {
            return Vec::new();
        };
        let mut settled = Vec::new();
        for block in &user.content {
            let ContentBlock::ToolResult { tool_use_id, .. } = block else {
                continue;
            };
            let Some(display) = self.0.remove(tool_use_id) else {
                continue;
            };
            let Ok(snapshot) = serde_json::from_value::<ArtifactRef>(display["artifact"].clone())
            else {
                continue;
            };
            let machine = display["machine"].as_str().map(str::to_owned);
            let checkpoint = WorkspaceCheckpoint {
                id: format!("checkpoint_{}", ids.operation()),
                prompt: prompt.clone(),
                assistant: assistant.clone(),
                tool_use: tool_use_id.clone(),
                snapshot,
                machine: machine.clone(),
            };
            // 远端的改动带机器（F98，remote-exec.md §25.4.5）。
            let change = FileChange {
                path: display["path"].as_str().unwrap_or_default().to_owned(),
                additions: display["additions"].as_u64().unwrap_or(0),
                deletions: display["deletions"].as_u64().unwrap_or(0),
                machine,
            };
            settled.push((checkpoint, change));
        }
        settled
    }
}

#[cfg(test)]
#[path = "checkpoint_tests.rs"]
mod tests;
