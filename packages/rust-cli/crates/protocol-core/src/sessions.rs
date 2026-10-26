//! 会话桥（docs/specs/rust-cli/app-protocol.md §3.3、§6.1）：经内核开会话，每个会话只订阅一次，事件交给协议的
//! 接收方；一批事件之后（窗口到期）让接收方发出。
//!
//! 每个会话一个泵任务：内核事件、窗口到期与协议排进来的工作（[`Bridged::run`]，例如订阅之后发首帧）都在这个任务里按顺序
//! 做，所以同一个会话的帧按计算的顺序写出，接收方不用跨 `.await` 持锁。
//!
//! 接收方先拿到 [`SessionView`]（`last_seq` 时刻的状态），再从 `last_seq` 之后接事件：中间没有空隙。内核的订阅流
//! 落后太多时会结束，这里从最后收到的 seq 重新订阅；连着两次没有新事件就结束（会话已经不在）。
//! 会话状态只归会话 actor：接收方只能保存由事件派生的东西（投影），不能当第二份事实。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use rust_cli_base::{Spawner, Task};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::session_api::{
    Kernel, KernelError, SequencedEvent, SessionHandle, SessionView,
};
use tokio::sync::mpsc;

use crate::serial::Job;

/// 会话事件的接收方。方法都在会话的泵任务里调用。
#[async_trait]
pub trait SessionSink: Send + Sync + 'static {
    /// 一个事件（按 seq 顺序）。
    async fn event(&self, event: SequencedEvent);

    /// 窗口到期（自上一次之后至少有一个事件）：把攒下的发出去。
    async fn flush(&self);
}

/// 一个打开着的会话。
pub struct Bridged<S> {
    pub handle: Arc<dyn SessionHandle>,
    pub sink: Arc<S>,
    jobs: mpsc::UnboundedSender<Job>,
    pump: Task<()>,
}

impl<S> Bridged<S> {
    /// 在这个会话的泵任务里排一件工作（排在已经收到的事件之后）。泵已经结束时丢弃。
    pub fn run(&self, job: Job) {
        let _ = self.jobs.send(job);
    }
}

impl<S> Drop for Bridged<S> {
    fn drop(&mut self) {
        self.pump.abort();
    }
}

type Opening<S> = Arc<tokio::sync::OnceCell<Result<Arc<Bridged<S>>, KernelError>>>;

/// 打开着的会话的表。
pub struct SessionBridge<S> {
    kernel: Arc<dyn Kernel>,
    spawner: Spawner,
    /// 一批事件的窗口。
    window: Duration,
    open: Mutex<HashMap<SessionId, Opening<S>>>,
}

impl<S: SessionSink> SessionBridge<S> {
    pub fn new(kernel: Arc<dyn Kernel>, spawner: Spawner, window: Duration) -> Self {
        Self {
            kernel,
            spawner,
            window,
            open: Mutex::new(HashMap::new()),
        }
    }

    pub fn kernel(&self) -> &Arc<dyn Kernel> {
        &self.kernel
    }

    fn table(&self) -> std::sync::MutexGuard<'_, HashMap<SessionId, Opening<S>>> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 已经打开的会话。
    pub fn get(&self, session: &SessionId) -> Option<Arc<Bridged<S>>> {
        let cell = self.table().get(session).cloned()?;
        cell.get().and_then(|opened| opened.as_ref().ok().cloned())
    }

    /// 打开会话（已经打开时直接给出）。`make` 用打开时的视图造接收方；之后的事件都交给它。同一个会话的并发打开
    /// 只开一次（后到的等先到的结果）；打开失败时不留记录，下次再试。
    pub async fn open<F>(
        &self,
        session: &SessionId,
        make: F,
    ) -> Result<Arc<Bridged<S>>, KernelError>
    where
        F: FnOnce(&Arc<dyn SessionHandle>, &SessionView) -> S,
    {
        let cell = self.table().entry(session.clone()).or_default().clone();
        let opened = cell
            .get_or_init(|| async move {
                let handle = self.kernel.open_session(session).await?;
                let view = handle.view().await?;
                let sink = Arc::new(make(&handle, &view));
                let (jobs, queue) = mpsc::unbounded_channel();
                let pump = self.spawner.spawn(
                    "session-pump",
                    pump(
                        handle.clone(),
                        sink.clone(),
                        queue,
                        view.last_seq,
                        self.window,
                    ),
                );
                Ok(Arc::new(Bridged {
                    handle,
                    sink,
                    jobs,
                    pump,
                }))
            })
            .await
            .clone();
        if opened.is_err() {
            let mut table = self.table();
            if table
                .get(session)
                .is_some_and(|known| Arc::ptr_eq(known, &cell))
            {
                table.remove(session);
            }
        }
        opened
    }

    /// 不再转发这个会话的事件（会话本身由内核管理）。
    pub fn close(&self, session: &SessionId) -> Option<Arc<Bridged<S>>> {
        let cell = self.table().remove(session)?;
        cell.get().and_then(|opened| opened.as_ref().ok().cloned())
    }

    pub fn sessions(&self) -> Vec<Arc<Bridged<S>>> {
        self.table()
            .values()
            .filter_map(|cell| cell.get().and_then(|opened| opened.as_ref().ok().cloned()))
            .collect()
    }
}

async fn pump<S: SessionSink>(
    handle: Arc<dyn SessionHandle>,
    sink: Arc<S>,
    mut jobs: mpsc::UnboundedReceiver<Job>,
    mut last: u64,
    window: Duration,
) {
    let mut empty = 0;
    loop {
        let mut stream = handle.subscribe(Some(last));
        let mut received = false;
        let mut deadline: Option<tokio::time::Instant> = None;
        loop {
            let due = deadline.unwrap_or_else(|| tokio::time::Instant::now() + window);
            tokio::select! {
                next = stream.next() => match next {
                    Some(event) => {
                        last = event.seq;
                        received = true;
                        sink.event(event).await;
                        if deadline.is_none() {
                            deadline = Some(tokio::time::Instant::now() + window);
                        }
                    }
                    None => break,
                },
                job = jobs.recv() => match job {
                    Some(job) => job.await,
                    // 句柄都已丢弃：会话关了。
                    None => return,
                },
                () = tokio::time::sleep_until(due), if deadline.is_some() => {
                    deadline = None;
                    sink.flush().await;
                }
            }
        }
        if deadline.is_some() {
            sink.flush().await;
        }
        empty = if received { 0 } else { empty + 1 };
        if empty >= 2 {
            tracing::debug!(
                event = "protocol.session.ended",
                session = handle.id().as_str(),
                "the session event stream ended"
            );
            // 流不会再有东西：之后的工作照常跑完。
            while let Some(job) = jobs.recv().await {
                job.await;
            }
            return;
        }
    }
}

#[cfg(test)]
#[path = "sessions_tests.rs"]
mod tests;
