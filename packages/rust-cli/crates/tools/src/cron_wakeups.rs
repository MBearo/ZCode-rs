//! 唤醒流与 `take_due`（docs/specs/rust-cli/tools-more.md §10.4、§16.4、§16.5）。
//!
//! 唤醒流不另起任务：actor 在 `select!` 里轮询它。它在任务表变化（watch）、任务文件变化（监听）或睡到
//! 期时醒来；到期时产出一项，水位线移到此刻，所以同一个到期时间只提醒一次（回合中被忽略的那次由回合
//! 结束时的 `start_next` 取走）。持久调度在会话第一次需要时开始：抢锁、订阅监听、读文件。

use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rust_cli_kernel_api::cron::{CronFire, CronSignals};
use rust_cli_kernel_api::ids::SessionId;
use tokio::sync::watch;

use crate::cron_board::CronBoard;
use crate::cron_durable::{belongs, first_sight};

impl CronBoard {
    /// 这个会话要不要开始持久调度：有存储、没关闭，且本进程用过定时任务或文件里本来就有任务（只在
    /// 唤醒流开始时读一次文件）。
    async fn wants_durable(&self, session: &SessionId, checked: &mut bool) -> bool {
        if self.store().is_none() {
            return false;
        }
        {
            let state = self.state();
            if state.schedulers.get(session).is_some_and(|s| s.closed) {
                return false;
            }
            if state.enabled {
                return true;
            }
        }
        if std::mem::replace(checked, true) {
            return false;
        }
        if self.read_durable().await.is_empty() {
            return false;
        }
        self.enable();
        true
    }

    /// 晚于水位线的最早时间：会话任务、错过提示、属于本会话的持久任务。
    fn next_wake(&self, session: &SessionId, mark: u64) -> Option<u64> {
        let mut state = self.state();
        let state = &mut *state;
        let mut times = Vec::new();
        times.extend(
            state
                .jobs
                .get(session)
                .into_iter()
                .flatten()
                .filter_map(|job| job.next_fire),
        );
        if let Some(scheduler) = state.schedulers.get_mut(session)
            && !scheduler.closed
        {
            times.extend(scheduler.missed_notice.as_ref().map(|(_, at)| *at));
            if scheduler.started {
                for task in &state.durable {
                    if belongs(task, session, scheduler.leader) {
                        let next = *scheduler
                            .next_fire
                            .entry(task.id.clone())
                            .or_insert_with(|| first_sight(self, task));
                        times.extend(next);
                    }
                }
            }
        }
        times.into_iter().filter(|at| *at > mark).min()
    }

    /// 不持锁的会话下一次探测锁的时间。
    fn probe_at(&self, session: &SessionId) -> Option<u64> {
        let state = self.state();
        let scheduler = state.schedulers.get(session)?;
        (scheduler.started && !scheduler.closed && !scheduler.leader)
            .then_some(scheduler.next_probe)
            .flatten()
    }
}

/// 监听流的下一项；没有监听或监听结束时一直等。
async fn next_change(changes: &mut Option<BoxStream<'static, ()>>) {
    let Some(stream) = changes else {
        return std::future::pending().await;
    };
    if stream.next().await.is_none() {
        *changes = None;
        std::future::pending::<()>().await;
    }
}

struct Wake {
    board: CronBoard,
    session: SessionId,
    versions: watch::Receiver<u64>,
    mark: u64,
    checked: bool,
    /// 这个流已经开始过持久调度（监听建不起来时不再重来）。
    started: bool,
    changes: Option<BoxStream<'static, ()>>,
}

enum Event {
    Timer,
    Version,
    File,
}

async fn next(mut wake: Wake) -> Option<((), Wake)> {
    loop {
        wake.versions.borrow_and_update();
        let (board, session) = (wake.board.clone(), wake.session.clone());
        if !wake.started && board.wants_durable(&session, &mut wake.checked).await {
            wake.started = true;
            wake.changes = board.start_durable(&session).await;
        }
        let now = board.now_ms();
        let due = board.next_wake(&session, wake.mark);
        if due.is_some_and(|at| at <= now) {
            wake.mark = now;
            return Some(((), wake));
        }
        let probe = board.probe_at(&session);
        let deadline = due.into_iter().chain(probe).min();
        let sleep = async {
            match deadline {
                Some(at) => tokio::time::sleep(Duration::from_millis(at.saturating_sub(now))).await,
                None => std::future::pending().await,
            }
        };
        let event = tokio::select! {
            () = sleep => Event::Timer,
            changed = wake.versions.changed() => match changed {
                Ok(()) => Event::Version,
                // 发送端随任务表一起存在；关闭说明任务表已释放。
                Err(_) => return None,
            },
            () = next_change(&mut wake.changes) => Event::File,
        };
        match event {
            Event::Timer if probe.is_some_and(|at| at <= board.now_ms()) => {
                board.probe(&session).await;
            }
            Event::File => {
                board.read_durable().await;
            }
            Event::Timer | Event::Version => {}
        }
    }
}

#[async_trait]
impl CronSignals for CronBoard {
    fn wakeups(&self, session: &SessionId) -> BoxStream<'static, ()> {
        // 修复（R1-1）：关闭把调度器标成关闭（挡住关闭时还在途的探测），以前没有地方复原——会话关闭后再打开
        // （新的 actor 订阅新的唤醒流），持久任务再也不触发。新的唤醒流就是会话又打开了：从头调度。
        self.state().schedulers.remove(session);
        let wake = Wake {
            board: self.clone(),
            session: session.clone(),
            versions: self.0.version.subscribe(),
            mark: 0,
            checked: false,
            started: false,
            changes: None,
        };
        futures_util::stream::unfold(wake, next).boxed()
    }

    /// 只在会话空闲时调用：先判断 keepalive，再取错过提示、到期的持久任务、到期的会话任务。
    async fn take_due(&self, session: &SessionId) -> Vec<CronFire> {
        self.keepalive(session);
        let now = self.now_ms();
        let mut fires = self.take_durable(session, now).await;
        fires.extend(self.take_session_jobs(session, now));
        if !fires.is_empty() {
            self.changed();
        }
        fires
    }

    /// 关闭（session-lifecycle.md §3.3）：停止持久调度、释放锁；会话任务与 loop 状态一并丢掉——会话再打开时按
    /// 从活动历史复原 CronCreate 建的任务（`resume`）。
    async fn session_closed(&self, session: &SessionId) {
        self.close_durable(session).await;
        let mut state = self.state();
        state.jobs.remove(session);
        state.loops.remove(session);
        drop(state);
        self.changed();
    }

    /// 用户中断：与 `ScheduleWakeup{stop: true}` 同样的清理，不记“循环已结束”（tools-more.md
    /// §18.1）。
    async fn cancel_loops(&self, session: &SessionId) -> usize {
        self.stop_loop(session)
    }

    /// resume：从活动历史复原 CronCreate 建的会话任务（tools-more.md §18.2）。
    async fn resume(&self, session: &SessionId, history: &[rust_cli_kernel_api::message::Message]) {
        self.restore(session, history);
    }
}
