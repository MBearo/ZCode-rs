//! 进程内的收件箱状态（docs/specs/rust-cli/cross-session.md §2.1）：绑定的主会话、还没 ack 的入站、发出去还没
//! 终态的 msg_id。没有队列：未 ack 的入站留在表里，`watch` 计数唤醒订阅者（与 agents 的 `Hub` 同一模式，README §6
//! 的进程内通道表）；锁不跨 await。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use futures_util::stream::{self, BoxStream};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::peers::{PeerAddress, PeerSignal, ReceiptStatus};
use tokio::sync::{oneshot, watch};

/// 发出去还没终态的消息最多记几条。
const OUTSTANDING_LIMIT: usize = 200;

struct Pending {
    signal: PeerSignal,
    /// 会话提交后通知连接的处理任务回复对方。
    done: Option<oneshot::Sender<()>>,
}

struct Outstanding {
    msg_id: String,
    to: PeerAddress,
    /// 收到过 held：之后只认终态。
    held: bool,
}

#[derive(Default)]
struct State {
    bound: Option<SessionId>,
    next_seq: u64,
    pending: Vec<Pending>,
    outstanding: VecDeque<Outstanding>,
    closed: bool,
}

pub(crate) struct Inbox {
    state: Mutex<State>,
    changed: watch::Sender<u64>,
}

impl Inbox {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::default(),
            changed: watch::Sender::new(0),
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn bump(&self) {
        self.changed.send_modify(|version| *version += 1);
    }

    /// 收到一条入站：分配 seq、放进表里；返回会话提交后完成的接收端。正在退出时为 None。
    pub(crate) fn push(
        &self,
        make: impl FnOnce(u64) -> PeerSignal,
    ) -> Option<oneshot::Receiver<()>> {
        let (done, wait) = oneshot::channel();
        {
            let mut state = self.state();
            if state.closed {
                return None;
            }
            state.next_seq += 1;
            let signal = make(state.next_seq);
            state.pending.push(Pending {
                signal,
                done: Some(done),
            });
        }
        self.bump();
        Some(wait)
    }

    /// 会话已提交这一项：从表里去掉并通知回复。哪个会话提交的都算（解绑之前收到的也已落库）。
    pub(crate) fn ack(&self, seq: u64) {
        let done = {
            let mut state = self.state();
            let at = state
                .pending
                .iter()
                .position(|item| item.signal.seq() == seq);
            at.and_then(|at| state.pending.remove(at).done)
        };
        if let Some(done) = done {
            let _ = done.send(());
        }
    }

    pub(crate) fn bind(&self, session: &SessionId) {
        self.state().bound = Some(session.clone());
        self.bump();
    }

    pub(crate) fn unbind(&self, session: &SessionId) {
        {
            let mut state = self.state();
            if state.bound.as_ref() == Some(session) {
                state.bound = None;
            }
        }
        self.bump();
    }

    pub(crate) fn bound(&self) -> Option<SessionId> {
        self.state().bound.clone()
    }

    /// 退出：不再收新的入站；表里没 ack 的丢掉（对方的连接随之关闭）。
    pub(crate) fn close(&self) {
        {
            let mut state = self.state();
            state.closed = true;
            state.pending.clear();
            state.bound = None;
        }
        self.bump();
    }

    /// `session` 是主会话时，seq 大于 `after` 的第一项。
    fn next_for(&self, session: &SessionId, after: u64) -> Option<PeerSignal> {
        let state = self.state();
        if state.bound.as_ref() != Some(session) {
            return None;
        }
        state
            .pending
            .iter()
            .find(|item| item.signal.seq() > after)
            .map(|item| item.signal.clone())
    }

    /// 这个会话的入站流：绑定之后按 seq 交出表里的项；没 ack 的在重新订阅时再交出（至少一次）。
    pub(crate) fn subscribe(
        self: &Arc<Self>,
        session: &SessionId,
    ) -> BoxStream<'static, PeerSignal> {
        let start = (
            self.clone(),
            session.clone(),
            self.changed.subscribe(),
            0_u64,
        );
        Box::pin(stream::unfold(
            start,
            |(inbox, session, mut changes, after)| async move {
                loop {
                    changes.borrow_and_update();
                    if let Some(signal) = inbox.next_for(&session, after) {
                        let seq = signal.seq();
                        return Some((signal, (inbox, session, changes, seq)));
                    }
                    if changes.changed().await.is_err() {
                        return None;
                    }
                }
            },
        ))
    }

    /// 记下一条发出去的消息（回执只认这些）。
    pub(crate) fn record_send(&self, msg_id: &str, to: &PeerAddress) {
        let mut state = self.state();
        if state.outstanding.len() >= OUTSTANDING_LIMIT {
            state.outstanding.pop_front();
        }
        state.outstanding.push_back(Outstanding {
            msg_id: msg_id.to_owned(),
            to: to.clone(),
            held: false,
        });
    }

    /// 发送失败：不再等它的回执。
    pub(crate) fn forget_send(&self, msg_id: &str) {
        self.state()
            .outstanding
            .retain(|item| item.msg_id != msg_id);
    }

    /// 回执对上一条发出去的消息时给出收件方的地址：held 之后只认终态，终态之后不再认。
    pub(crate) fn match_receipt(&self, msg_id: &str, status: ReceiptStatus) -> Option<PeerAddress> {
        let mut state = self.state();
        let at = state
            .outstanding
            .iter()
            .position(|item| item.msg_id == msg_id)?;
        let item = &mut state.outstanding[at];
        if item.held && status == ReceiptStatus::Held {
            return None;
        }
        let to = item.to.clone();
        if status == ReceiptStatus::Held {
            item.held = true;
        } else {
            state.outstanding.remove(at);
        }
        Some(to)
    }
}

#[cfg(test)]
#[path = "inbox_tests.rs"]
mod tests;
