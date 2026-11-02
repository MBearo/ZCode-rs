//! 一次运行的提交核心（工作线 F130，architecture-review.md C2、D5）：主会话的 actor 与子代理的 sink 共用这一份。
//!
//! 记三样跨提交的东西：
//! - 当前轮：随每一批交给存储，存储不再逐行从库里推导（node-store.md §4，修复线 F6）；批里有新的人类输入时换轮，
//!   提交成功后才记下；
//! - 工具已结束、结果还没提交的 checkpoint（session-lifecycle.md §4）：主循环先发 `ToolFinished`、再提交工具结果；
//! - 同样时机的子代理关联（Agent 工具的 display 带子会话时，node-store.md §4.6）。
//!
//! 纯状态，没有 IO：调用方用 [`RunLedger::settle`] 拿到一条消息的同批事实，用 [`RunLedger::batch`] 包成一批交给存储，
//! 成功后调 [`RunLedger::committed`]。以前这些规则写了三份（会话 actor、子代理的 sink、子会话的开头），子代理那份没有
//! 关联：子代理再派生的子代理在子会话里找不到（D5）。

use std::collections::HashMap;

use serde_json::Value;

use crate::checkpoint::DISPLAY_KEY;
use crate::clock::Ids;
use crate::ids::{CommandId, MessageId, OwnerEpoch, ToolUseId};
use crate::message::{ContentBlock, Message, UserOrigin};
use crate::session_api::FileChange;
use crate::store::{ArtifactRef, Fact, FactBatch, TurnAnchor, WorkspaceCheckpoint};
use crate::tool::ToolOutput;

/// 一次运行（主会话的回合、一个子代理的运行）提交时要配对的状态。
#[derive(Debug, Default)]
pub struct RunLedger {
    turn: Option<TurnAnchor>,
    /// 工具已结束、结果还没提交的 checkpoint（`ToolOutput.display.checkpoint` 原样）。
    checkpoints: HashMap<ToolUseId, Value>,
    links: HashMap<ToolUseId, Fact>,
}

/// 一条消息带出的同批事实与改动摘要。
#[derive(Debug)]
pub struct Settled {
    /// 这一批的事实，顺序是消息 → checkpoint → 子代理关联 → 计量（usage.md §2.5）。
    pub facts: Vec<Fact>,
    /// 落库后进入会话 checkpoint 清单的 checkpoint 与它的改动摘要（主会话据此发 `FileChanges`）。
    pub checkpoints: Vec<(WorkspaceCheckpoint, FileChange)>,
}

impl RunLedger {
    /// 从存储里的当前轮开始（冷加载的 `LoadedSession.turn`；新会话没有）。
    pub fn new(turn: Option<TurnAnchor>) -> Self {
        Self {
            turn,
            ..Self::default()
        }
    }

    /// 已提交的当前轮。
    pub fn turn(&self) -> Option<&TurnAnchor> {
        self.turn.as_ref()
    }

    /// 工具结束（`QueryEvent::ToolFinished`，先于它的结果提交到达）：记下它带出的 checkpoint 与子代理关联。
    ///
    /// 只有写入真的发生了才有 checkpoint；结果被中断文本替换时主循环仍把它带在输出里（query.md 的执行器），所以这里
    /// 不看 `is_error`。
    pub fn tool_finished(&mut self, tool_use: &ToolUseId, output: &ToolOutput) {
        let checkpoint = &output.display[DISPLAY_KEY];
        if checkpoint.is_object() {
            self.checkpoints
                .insert(tool_use.clone(), checkpoint.clone());
        }
        if let Some(link) = crate::agents::subagent_link(tool_use, output) {
            self.links.insert(tool_use.clone(), link);
        }
    }

    /// 一条消息与它的同批事实：工具结果带出记下的 checkpoint 与子代理关联，`usage` 排在最后。`prompt` 是这次运行开头
    /// 的 user 消息（checkpoint 的 `messageId`）；没有时（压缩的运行）checkpoint 留着不结算，关联照常。
    pub fn settle(
        &mut self,
        message: Message,
        prompt: Option<&MessageId>,
        usage: Vec<Fact>,
        ids: &dyn Ids,
    ) -> Settled {
        let checkpoints = match prompt {
            Some(prompt) => self.take_checkpoints(&message, prompt, ids),
            None => Vec::new(),
        };
        let links = self.take_links(&message);
        let facts = std::iter::once(Fact::MessageAppended { message })
            .chain(
                checkpoints
                    .iter()
                    .map(|(checkpoint, _)| Fact::CheckpointCreated {
                        checkpoint: checkpoint.clone(),
                    }),
            )
            .chain(links)
            .chain(usage)
            .collect();
        Settled { facts, checkpoints }
    }

    /// 这条工具结果消息带出的 checkpoint 与改动摘要；不是工具结果的消息没有。
    fn take_checkpoints(
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
            let Some(display) = self.checkpoints.remove(tool_use_id) else {
                continue;
            };
            let Ok(snapshot) = serde_json::from_value::<ArtifactRef>(display["artifact"].clone())
            else {
                continue;
            };
            let machine =
                crate::ids::MachineName::new(display["machine"].as_str().unwrap_or_default());
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

    /// 这条工具结果消息里各个调用记下的子代理关联。
    fn take_links(&mut self, message: &Message) -> Vec<Fact> {
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

    /// 交给存储的一批：当前轮按批里的事实推进（[`TurnAnchor::after`]），提交成功后调 [`Self::committed`]。
    pub fn batch(&self, command: Option<CommandId>, facts: Vec<Fact>, ids: &dyn Ids) -> FactBatch {
        let turn = TurnAnchor::after(self.turn.as_ref(), &facts, || ids.turn());
        FactBatch {
            owner_epoch: OwnerEpoch::LOCAL,
            command,
            facts,
            turn,
        }
    }

    /// 存储接受了这一批（`turn` 是 [`Self::batch`] 给出的那一批的轮次）。
    pub fn committed(&mut self, turn: Option<TurnAnchor>) {
        self.turn = turn;
    }
}

#[cfg(test)]
#[path = "run_ledger_tests.rs"]
mod tests;
