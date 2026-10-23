//! next 通道（L18，docs/specs/rust-cli/query.md §5）：插话输入与两种后台通知按到达顺序排在一起。
//!
//! 插话、后台命令通知与子代理通知都是优先级 `next`，工具边界按队列顺序取，回合
//! 结束后也按这个顺序开回合；`Queue` 输入是 `later`，不进通道。通道只在 actor 里经 `&mut` 访问；输入的
//! 正文仍在 `Actor::queue`（QueueChanged 的唯一来源），通道里只记它的命令 ID。
//!
//! 修正（L18）：以前通知只在回合结束后各开一个回合（按缺省优先级 `later`
//! 理解），排在全部排队输入之后；通知应是 `next`，在下一个工具边界并入。

use rust_cli_kernel_api::agents::AgentNotice;
use rust_cli_kernel_api::env_shell::ShellNotice;
use rust_cli_kernel_api::ids::CommandId;
use rust_cli_kernel_api::message::{ContentBlock, Message, UserOrigin};
use rust_cli_query::task_notification;

use crate::actor::Actor;
use crate::notices::NOTICE_SOURCE;
use crate::peers::{PEER_SOURCE, PeerItem};
use crate::turn::TurnInput;

/// 一条等候中的后台通知，或跨会话的消息与回执（同伴消息同样是优先级 `next`，cross-session.md §1.7）。
#[derive(Debug, Clone)]
pub(crate) enum Waiting {
    Shell(ShellNotice),
    Agent(AgentNotice),
    Peer(PeerItem),
}

impl Waiting {
    /// 通知全文（`<system-reminder>` 包装）：工具边界并入用它；后台通知开回合也用它。
    pub(crate) fn text(&self) -> String {
        match self {
            Self::Shell(notice) => task_notification(notice),
            Self::Agent(notice) => notice.text.clone(),
            Self::Peer(item) => item.text(),
        }
    }

    /// 空闲时开回合的 user 消息文字（同伴消息的第一行不同）。
    pub(crate) fn turn_text(&self) -> String {
        match self {
            Self::Peer(item) => item.turn_text(),
            _ => self.text(),
        }
    }

    /// 开回合的 meta user 消息的来源。
    pub(crate) fn source(&self) -> &'static str {
        match self {
            Self::Peer(_) => PEER_SOURCE,
            _ => NOTICE_SOURCE,
        }
    }

    /// 只在工具边界并入、不开回合（`-p` 的回执，passive）。
    pub(crate) fn passive(&self) -> bool {
        matches!(self, Self::Peer(item) if item.passive())
    }

    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Shell(a), Self::Shell(b)) => a.seq == b.seq,
            (Self::Agent(a), Self::Agent(b)) => a.seq == b.seq,
            (Self::Peer(a), Self::Peer(b)) => a.key() == b.key(),
            _ => false,
        }
    }
}

/// 通道里的一项。
#[derive(Debug, Clone)]
pub(crate) enum Lane {
    /// 插话（或 SessionStart 给的首条输入）：正文在 `Actor::queue` 里。
    Input(CommandId),
    Notice(Waiting),
}

/// 消息里的通知全文：开回合的 meta user 消息，或工具边界并入的 `task-notification` attachment。
fn notice_text(message: &Message) -> Option<&str> {
    match message {
        Message::User(user) if matches!(&user.origin, UserOrigin::Meta { source } if source == NOTICE_SOURCE || source == PEER_SOURCE) => {
            match user.content.as_slice() {
                [ContentBlock::Text { text }] => Some(text),
                _ => None,
            }
        }
        Message::Attachment(attachment)
            if attachment.source == "queued_command"
                && attachment.data["commandMode"] == NOTICE_SOURCE =>
        {
            Some(&attachment.text)
        }
        _ => None,
    }
}

impl Actor {
    /// 通知到达：已在对话里或已在通道里（重复送达）时只 ack；否则排进通道。返回是否新排进。
    pub(crate) async fn enqueue_notice(&mut self, notice: Waiting) -> bool {
        let text = notice.text();
        let seen = self
            .messages
            .iter()
            .any(|message| notice_text(message) == Some(text.as_str()));
        let waiting = self
            .lane
            .iter()
            .any(|item| matches!(item, Lane::Notice(queued) if queued.same(&notice)));
        if seen || waiting {
            if !waiting {
                self.ack_notice(&notice).await;
            }
            return false;
        }
        self.lane.push_back(Lane::Notice(notice));
        true
    }

    /// 提交之后才 ack（至少一次）。同伴的消息与回执在收到时已经提交并 ack（peers.rs）。
    pub(crate) async fn ack_notice(&self, notice: &Waiting) {
        match notice {
            Waiting::Shell(notice) => self.ack(notice.seq).await,
            Waiting::Agent(notice) => self.ack_agent(notice.seq).await,
            Waiting::Peer(_) => {}
        }
    }

    /// 工具边界并入的 `task-notification` 已提交：从通道去掉这条通知并 ack。
    pub(crate) async fn notice_absorbed(&mut self, message: &Message) {
        if !matches!(message, Message::Attachment(_)) {
            return;
        }
        let Some(text) = notice_text(message) else {
            return;
        };
        let at = self
            .lane
            .iter()
            .position(|item| matches!(item, Lane::Notice(notice) if notice.text() == text));
        if let Some(Lane::Notice(notice)) = at.and_then(|at| self.lane.remove(at)) {
            self.ack_notice(&notice).await;
        }
    }

    /// 从队列与通道去掉一条输入。
    pub(crate) fn remove_input(&mut self, command: &CommandId) {
        self.queue.retain(|input| input.command != *command);
        self.lane
            .retain(|item| !matches!(item, Lane::Input(queued) if queued == command));
    }

    /// 回合结束后的下一项（query.md §5）：通道头是输入就是它；是通知就取走头部连续的通知，合成一个
    /// 回合（通知合并）；通道空时是 `Queue` 输入（优先级 `later`）。定时任务由调用方最后看。
    pub(crate) fn next_turn_input(&mut self) -> Option<TurnInput> {
        loop {
            // 只在工具边界并入的项（`-p` 的回执）不开回合：留在通道里，看后面的项。
            let front = self
                .lane
                .iter()
                .position(|item| !matches!(item, Lane::Notice(notice) if notice.passive()));
            if let Some(at) = front
                && at > 0
            {
                let skipped: Vec<Lane> = self.lane.drain(..at).collect();
                let next = self.next_turn_input();
                for item in skipped.into_iter().rev() {
                    self.lane.push_front(item);
                }
                return next;
            }
            if front.is_none() && !self.lane.is_empty() {
                // 通道里只剩不开回合的项：照常看 `Queue` 输入。
                let kept: Vec<Lane> = self.lane.drain(..).collect();
                let next = self.next_turn_input();
                for item in kept.into_iter().rev() {
                    self.lane.push_front(item);
                }
                return next;
            }
            match self.lane.front() {
                Some(Lane::Input(command)) => {
                    let command = command.clone();
                    let input = self
                        .queue
                        .iter()
                        .find(|input| input.command == command)
                        .cloned();
                    self.remove_input(&command);
                    self.emit_queue();
                    // 通道里的输入总在队列里（两处同时增删）；万一不在就看下一项。
                    let Some(input) = input else { continue };
                    return Some(TurnInput::Human {
                        screened: self.screened.remove(&input.command).flatten(),
                        command: input.command,
                        text: input.text,
                    });
                }
                Some(Lane::Notice(_)) => {
                    let mut notices = Vec::new();
                    while let Some(Lane::Notice(notice)) = self.lane.front() {
                        if notice.passive() {
                            break;
                        }
                        if let Some(Lane::Notice(notice)) = self.lane.pop_front() {
                            notices.push(notice);
                        }
                    }
                    return Some(TurnInput::Notices(notices));
                }
                None => {
                    let next = self.queue.pop_front()?;
                    self.emit_queue();
                    match self.queued_kinds.remove(&next.command) {
                        Some(crate::bash::QueuedKind::Bash(line)) => {
                            return Some(TurnInput::Bash {
                                command: next.command,
                                line,
                            });
                        }
                        Some(crate::bash::QueuedKind::Content(content)) => {
                            return Some(TurnInput::Content {
                                command: next.command,
                                content,
                            });
                        }
                        None => {}
                    }
                    return Some(TurnInput::Human {
                        screened: self.screened.remove(&next.command).flatten(),
                        command: next.command,
                        text: next.text,
                    });
                }
            }
        }
    }
}
