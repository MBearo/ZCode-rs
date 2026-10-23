//! 文件 checkpoint 的落库（docs/specs/rust-cli/session-lifecycle.md §4）。
//!
//! 工具在写入后把 artifact 引用放进 `ToolOutput.display.checkpoint`；`ToolFinished` 先于工具结果
//! 的提交到达 actor（同一个通道），所以这里先记下，等结果提交时与它在一个事务里落库。
//!
//! 子代理关联（修复线 F2，node-store.md §4.6）走同一条路：Agent 工具的 display 带子会话时记下
//! `Fact::SubagentLinked`，排在这条工具结果之后同批提交。

use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::message::{ContentBlock, Message};
use rust_cli_kernel_api::session_api::{FileChange, KernelEvent};
use rust_cli_kernel_api::store::{Fact, WorkspaceCheckpoint};
use rust_cli_kernel_api::tool::ToolOutput;

use crate::actor::Actor;

impl Actor {
    /// 工具结束：有 checkpoint 时记下，等它的结果提交（配对规则在 `kernel_api::checkpoint`，子代理的
    /// sink 用同一份）。
    pub(crate) fn unsettle(&mut self, tool_use: &ToolUseId, output: &ToolOutput) {
        self.unsettled.note(tool_use, output);
        if let Some(link) = rust_cli_kernel_api::agents::subagent_link(tool_use, output) {
            self.links.insert(tool_use.clone(), link);
        }
    }

    /// 这条工具结果消息带出的 checkpoint 与要一起提交的事实。
    pub(crate) fn settle_checkpoints(
        &mut self,
        message: &Message,
    ) -> (Vec<(WorkspaceCheckpoint, FileChange)>, Vec<Fact>) {
        let links = self.settle_links(message);
        let Some(prompt) = self.running.as_ref().and_then(|r| r.prompt.clone()) else {
            return (Vec::new(), links);
        };
        let settled = self
            .unsettled
            .settle(message, &prompt, self.deps.ids.as_ref());
        let mut facts: Vec<Fact> = settled
            .iter()
            .map(|(checkpoint, _)| Fact::CheckpointCreated {
                checkpoint: checkpoint.clone(),
            })
            .collect();
        facts.extend(links);
        (settled, facts)
    }

    /// 这条工具结果消息里各个调用记下的子代理关联。
    fn settle_links(&mut self, message: &Message) -> Vec<Fact> {
        let Message::User(user) = message else {
            return Vec::new();
        };
        user.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolResult { tool_use_id, .. } => self.links.remove(tool_use_id),
                _ => None,
            })
            .collect()
    }

    /// 提交成功后：进入清单，发 `FileChanges`。
    pub(crate) fn checkpoints_settled(&mut self, settled: Vec<(WorkspaceCheckpoint, FileChange)>) {
        let Some(turn) = self.running.as_ref().map(|r| r.turn.clone()) else {
            return;
        };
        if settled.is_empty() {
            return;
        }
        let mut files = Vec::new();
        for (checkpoint, change) in settled {
            self.checkpoints.push(checkpoint);
            files.push(change);
        }
        self.log.append(KernelEvent::FileChanges {
            turn,
            files,
            rewindable: true,
        });
    }
}
