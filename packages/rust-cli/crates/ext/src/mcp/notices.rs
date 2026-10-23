//! 读者交给连接任务的通知（docs/specs/rust-cli/ext.md §3.3“通道与溢出”）。
//!
//! 缺陷修复（审查 R1-18）：原来是无界通道，服务器连续发 `list_changed` 而连接任务正在拉清单时，通知无限堆积。
//! 这里没有队列，状态本身有界：
//! - `list_changed` 按种类合并：同一种还没取走时不再加一条（收到后连接任务重新拉整份清单，可重建）；
//! - 关闭只有一次，一定送达（排在还没取走的 `list_changed` 之后）。
//!
//! 发送方从不等待：读者是这条连接上事件的唯一消费者，连接任务拉清单时正等着读者送来响应，读者若在这里等连接
//! 任务就成了互相等待。

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::watch;

use super::rpc::Notice;

#[derive(Default)]
struct Pending {
    /// 还没取走的 `list_changed` 种类，按第一次到达的顺序。
    changed: Vec<String>,
    closed: Option<Notice>,
}

fn lock(pending: &Mutex<Pending>) -> MutexGuard<'_, Pending> {
    pending.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 读者一侧。释放时接收方读完剩下的就结束。
pub(crate) struct NoticeSender {
    pending: Arc<Mutex<Pending>>,
    wake: watch::Sender<()>,
}

/// 连接任务一侧。
pub(crate) struct NoticeReceiver {
    pending: Arc<Mutex<Pending>>,
    wake: watch::Receiver<()>,
}

pub(crate) fn channel() -> (NoticeSender, NoticeReceiver) {
    let pending = Arc::new(Mutex::default());
    let (wake, woken) = watch::channel(());
    (
        NoticeSender {
            pending: pending.clone(),
            wake,
        },
        NoticeReceiver {
            pending,
            wake: woken,
        },
    )
}

impl NoticeSender {
    pub(crate) fn send(&self, notice: Notice) {
        {
            let mut pending = lock(&self.pending);
            match notice {
                Notice::ListChanged(kind) => {
                    if !pending.changed.contains(&kind) {
                        pending.changed.push(kind);
                    }
                }
                closed @ Notice::Closed { .. } => pending.closed = Some(closed),
            }
        }
        self.wake.send_replace(());
    }
}

impl NoticeReceiver {
    fn take(&self) -> Option<Notice> {
        let mut pending = lock(&self.pending);
        if pending.changed.is_empty() {
            pending.closed.take()
        } else {
            Some(Notice::ListChanged(pending.changed.remove(0)))
        }
    }

    /// 下一条通知；发送方已释放且没有剩下的通知时为 None。
    pub(crate) async fn recv(&mut self) -> Option<Notice> {
        loop {
            self.wake.borrow_and_update();
            if let Some(notice) = self.take() {
                return Some(notice);
            }
            if self.wake.changed().await.is_err() {
                return self.take();
            }
        }
    }
}

#[cfg(test)]
#[path = "notices_tests.rs"]
mod tests;
