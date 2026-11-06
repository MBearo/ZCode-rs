//! 对话追加的唯一入口（工作线 F130，architecture-review.md C5）：[`Actor::append`] 先落库，成功后 [`Actor::after_commit`]
//! 跑提交后的观察者、让消息进历史、发事件。主循环的提交（turn.rs 的 `record`）、通知与定时任务开的回合（turn.rs）、
//! SessionStart 的 hook 消息（hooks.rs）与本地命令的记录（local_record.rs）都经这里。
//!
//! 以前这 5 条路径各写一遍“提交 → 进历史 → 发 `MessageDone`”，调用的观察者各不相同（6 个、3 个、0 个）。观察者都按消息
//! 内容过滤（计划模式的两个提醒、`goal_status`、工具边界并入的通知、本回合开头的输入、带用量的 assistant、压缩边界），
//! 别的路径上的消息不会命中，所以统一调用不改行为。

use rust_cli_kernel_api::ids::CommandId;
use rust_cli_kernel_api::message::{Message, SystemMessage};
use rust_cli_kernel_api::session_api::{FileChange, KernelEvent};
use rust_cli_kernel_api::store::{Fact, StoreError, WorkspaceCheckpoint};

use crate::actor::Actor;

/// 已落库、还没进历史的消息与它们带出的 checkpoint（[`Actor::after_commit`] 的输入）。
pub(crate) struct Appended {
    messages: Vec<Message>,
    checkpoints: Vec<(WorkspaceCheckpoint, FileChange)>,
}

impl Actor {
    /// 一批消息与计量一个事务：每条消息带出的 checkpoint 与子代理关联排在它后面（kernel-api `RunLedger`），工具边界
    /// 并入的同伴项的 `peer_merged` 跟在它的 attachment 后面（F139），`tail` 排在最后：计量（usage.md §2.5），或开回合
    /// 并入的同伴项的 `peer_merged`（turn.rs）。成功后调用方调 [`Self::after_commit`]（通知回合在两者之间发
    /// `TurnStarted`）。
    pub(crate) async fn append(
        &mut self,
        command: Option<CommandId>,
        messages: Vec<Message>,
        tail: Vec<Fact>,
    ) -> Result<Appended, StoreError> {
        let prompt = self.running.as_ref().and_then(|r| r.prompt.clone());
        let mut facts = Vec::new();
        let mut checkpoints = Vec::new();
        for message in &messages {
            let settled = self.ledger.settle(
                message.clone(),
                prompt.as_ref(),
                Vec::new(),
                self.deps.ids.as_ref(),
            );
            facts.extend(settled.facts);
            checkpoints.extend(settled.checkpoints);
            // 工具边界并入的同伴消息与回执：“已并入”的事实与这条 attachment 同一批（cross-session.md §2.4，F139），
            // 中间失败时两者都不在。
            if let Some(waiting) = self.inbox.absorbing_item(message.id()) {
                facts.extend(waiting.merged_facts());
            }
        }
        facts.extend(tail);
        self.commit(command, facts).await?;
        Ok(Appended {
            messages,
            checkpoints,
        })
    }

    /// 提交成功之后：逐条跑观察者、进历史、发 `MessageDone`；压缩边界之后清 `/loop` 的说明、发 `CompactDone`；带用量的
    /// assistant 与压缩之后重算上下文提示；最后 checkpoint 进清单、发 `FileChanges`。
    pub(crate) async fn after_commit(&mut self, appended: Appended) {
        let Appended {
            messages,
            checkpoints,
        } = appended;
        let mut boundary = None;
        for message in messages {
            self.plan_attachment_committed(&message);
            self.goal_committed(&message);
            // 工具边界并入的通知：提交后才从通道去掉并 ack（lane.rs）。
            self.notice_absorbed(&message).await;
            // 本回合开头的人类输入：之前没有 user 消息时是首个回合（会话标题，title.rs）。本地命令的记录不算（修复线
            // L-A：`/clear` 之后的新会话以 `/clear` 的记录开头，第一条输入照样起标题）。
            let prompt = self
                .running
                .as_ref()
                .is_some_and(|running| running.prompt.as_ref() == Some(message.id()));
            let first = !self
                .messages
                .iter()
                .any(|m| matches!(m, Message::User(user) if !crate::local_record::is_record(user)));
            if let Message::System(SystemMessage::CompactBoundary(compacted)) = &message {
                boundary = Some(compacted.id.clone());
            }
            // 缺陷修复（R5-8）：原来内核从不发上下文用量，footer 的上下文提示永远不出现。在每条带用量的 assistant
            // 之后重算；提示的级别与文字选择由压缩模块给出（compact.md §2.1，F14 起是 ContextNotice）。F27 起同一处另发
            // `(token, 有效窗口)`，给 footer 的 ctx。
            let usage =
                matches!(&message, Message::Assistant(assistant) if assistant.usage.is_some());
            self.messages.push(message.clone());
            if prompt {
                self.after_prompt(&message, first);
            }
            self.log.append(KernelEvent::MessageDone { message });
            if usage {
                self.context_changed();
            }
        }
        if let Some(boundary) = boundary {
            // 压缩后自主说明已不在上下文里：下一次 `/loop` 触发重新发全文（压缩后的清理）。
            self.reset_loop_ticks();
            self.log.append(KernelEvent::CompactDone { boundary });
            // 缺陷修复（F14）：上下文提示应在消息变化时重算；以前只在提交 assistant 时算，压缩后到下一条回复之前仍显示
            // 压缩前的百分比（compact.md §2.1）。F27 起 footer 的 ctx 同一处重算。
            self.context_changed();
        }
        self.checkpoints_settled(checkpoints);
    }
}
