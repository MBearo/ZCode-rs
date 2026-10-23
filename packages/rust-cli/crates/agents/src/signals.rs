//! 发给会话 actor 的信号（docs/specs/rust-cli/agents.md §5、§5.3）：后台子代理的终态通知与任务清单变化。
//!
//! 通知在 ack 之前一直留在这里：重新订阅时再次送达（至少一次）；会话 actor 提交消息后才 ack。
//!
//! 缺陷修复（审查 R1-18）：原来每个订阅者一个无界通道，消费方（会话 actor）慢时信号在通道里无限堆积，订阅者
//! 不再读又不释放流时也一样。现在没有队列：订阅流按会话的状态拉取（与 env-local 的后台命令通知相同的模式），
//! 状态本身有界——
//! - 通知是必须送达的事实，留到 ack（数量就是还没处理的后台子代理数），一条都不丢；
//! - 任务清单是可重建的快照，只留最新一份，消费方没读到的旧版本直接被新版本替换。
//!
//! 两类信号按到达顺序交出（共用一个顺序号），替换掉的任务清单不再占位置。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rust_cli_kernel_api::agents::{AgentNotice, AgentSignal};
use rust_cli_kernel_api::ids::{AgentId, SessionId};
use serde_json::Value;
use tokio::sync::watch;

#[derive(Default)]
struct Channel {
    /// 通知的 seq（ack 用，从 1 起连续编号）。
    next_seq: u64,
    /// 通知与任务清单共用的到达顺序号。
    next_order: u64,
    /// 还没 ack 的通知与它的到达顺序。
    pending: Vec<(u64, AgentNotice)>,
    /// 最新的任务清单与它的到达顺序。
    tasks: Option<(u64, Value)>,
}

impl Channel {
    fn order(&mut self) -> u64 {
        self.next_order += 1;
        self.next_order
    }
}

struct Inner {
    channels: Mutex<HashMap<SessionId, Channel>>,
    /// 任何会话有新信号时加一；订阅流据此醒来，再查自己的会话。
    changed: watch::Sender<u64>,
}

#[derive(Clone)]
pub(crate) struct Hub(Arc<Inner>);

impl Default for Hub {
    fn default() -> Self {
        Self(Arc::new(Inner {
            channels: Mutex::default(),
            changed: watch::channel(0).0,
        }))
    }
}

/// 一个订阅已经交出的位置。
struct Cursor {
    inner: Arc<Inner>,
    session: SessionId,
    /// 已交出的最大到达顺序。
    delivered: u64,
    /// 订阅之前已有的任务清单不交出（原来只送给当时的订阅者）。
    tasks_before: u64,
}

fn lock(inner: &Inner) -> MutexGuard<'_, HashMap<SessionId, Channel>> {
    inner
        .channels
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// 下一个要交出的信号：通知与任务清单里到达顺序最小、还没交出的一个。
fn next_signal(channel: &Channel, cursor: &Cursor) -> Option<(u64, AgentSignal)> {
    let notice = channel
        .pending
        .iter()
        .find(|(order, _)| *order > cursor.delivered)
        .map(|(order, notice)| (*order, AgentSignal::Notice(notice.clone())));
    let tasks = channel
        .tasks
        .as_ref()
        .filter(|(order, _)| *order > cursor.delivered.max(cursor.tasks_before))
        .map(|(order, tasks)| {
            let tasks = tasks.clone();
            (*order, AgentSignal::TasksChanged { tasks })
        });
    match (notice, tasks) {
        (Some(notice), Some(tasks)) => Some(if notice.0 < tasks.0 { notice } else { tasks }),
        (one, other) => one.or(other),
    }
}

async fn next(mut cursor: Cursor) -> Option<(AgentSignal, Cursor)> {
    let mut changed = cursor.inner.changed.subscribe();
    loop {
        changed.borrow_and_update();
        let found = lock(&cursor.inner)
            .get(&cursor.session)
            .and_then(|channel| next_signal(channel, &cursor));
        if let Some((order, signal)) = found {
            cursor.delivered = order;
            return Some((signal, cursor));
        }
        // 发送方在 `Inner` 里，订阅流持有它，所以不会关闭。
        if changed.changed().await.is_err() {
            return None;
        }
    }
}

impl Hub {
    fn channels(&self) -> MutexGuard<'_, HashMap<SessionId, Channel>> {
        lock(&self.0)
    }

    fn wake(&self) {
        self.0.changed.send_modify(|count| *count += 1);
    }

    /// 先给出还没有 ack 的通知，再给新的信号。
    pub(crate) fn subscribe(&self, session: &SessionId) -> BoxStream<'static, AgentSignal> {
        let tasks_before = self
            .channels()
            .get(session)
            .and_then(|channel| channel.tasks.as_ref().map(|(order, _)| *order))
            .unwrap_or(0);
        let cursor = Cursor {
            inner: self.0.clone(),
            session: session.clone(),
            delivered: 0,
            tasks_before,
        };
        futures_util::stream::unfold(cursor, next).boxed()
    }

    /// 记下一条通知并唤醒订阅者；返回它的 seq。
    pub(crate) fn notify(
        &self,
        session: &SessionId,
        agent: AgentId,
        status: &str,
        text: String,
    ) -> u64 {
        let seq = {
            let mut channels = self.channels();
            let channel = channels.entry(session.clone()).or_default();
            channel.next_seq += 1;
            let notice = AgentNotice {
                seq: channel.next_seq,
                agent,
                status: status.to_owned(),
                text,
            };
            let order = channel.order();
            channel.pending.push((order, notice));
            channel.next_seq
        };
        self.wake();
        seq
    }

    /// 换上最新的任务清单（旧的没交出就被替换：快照可重建）。
    pub(crate) fn tasks_changed(&self, session: &SessionId, tasks: Value) {
        {
            let mut channels = self.channels();
            let channel = channels.entry(session.clone()).or_default();
            let order = channel.order();
            channel.tasks = Some((order, tasks));
        }
        self.wake();
    }

    pub(crate) fn ack(&self, session: &SessionId, seq: u64) {
        if let Some(channel) = self.channels().get_mut(session) {
            channel.pending.retain(|(_, notice)| notice.seq != seq);
        }
    }

    /// 通知还没有 ack 的子代理。
    pub(crate) fn pending(&self, session: &SessionId) -> Vec<AgentId> {
        self.channels()
            .get(session)
            .map_or_else(Vec::new, |channel| {
                channel
                    .pending
                    .iter()
                    .map(|(_, notice)| notice.agent.clone())
                    .collect()
            })
    }
}

#[cfg(test)]
#[path = "signals_tests.rs"]
mod tests;
