//! 持久定时任务：文件的投影、每个会话的调度状态、错过的一次性任务（docs/specs/rust-cli/tools-more.md §16）。
//!
//! 唯一事实源是文件（经 [`CronStore`]）；投影只在本进程写完之后、监听产出变化时、工具要读时重读，重读是
//! 幂等的。哪个会话触发哪个任务见 [`belongs`]。

use rust_cli_kernel_api::cron::{CronFire, CronFireKind};
use rust_cli_kernel_api::cron_store::{CronCreator, CronTasksChange, NewCronTask, StoredCronTask};
use rust_cli_kernel_api::ids::SessionId;

use crate::cron_board::{CronBoard, State};
use crate::cron_expr::{self, RECURRING_MAX_AGE_MS};

/// 不持锁的会话探测锁的间隔。
pub(crate) const PROBE_INTERVAL_MS: u64 = 5_000;
/// 错过提示的任务 ID（不是文件里的任务）。
pub(crate) const MISSED_JOB: &str = "missed";

/// 持久任务是否由会话 `session` 触发（tools-more.md §16.3）。
pub(crate) fn belongs(task: &StoredCronTask, session: &SessionId, leader: bool) -> bool {
    match &task.created_by {
        None => leader,
        Some(creator) if creator == session => true,
        Some(_) if task.creator == CronCreator::ThisProcess => true,
        Some(_) => leader && task.in_project && task.creator == CronCreator::Gone,
    }
}

impl CronBoard {
    /// 从文件重读投影（只留 cron 合法的项），返回投影；读失败时保留原投影。
    pub(crate) async fn read_durable(&self) -> Vec<StoredCronTask> {
        let Some(store) = self.store() else {
            return Vec::new();
        };
        let tasks = match store.tasks().await {
            Ok(tasks) => tasks,
            Err(error) => {
                tracing::warn!(%error, "cannot read scheduled tasks");
                return self.state().durable.clone();
            }
        };
        let valid: Vec<StoredCronTask> = tasks
            .into_iter()
            .filter(|task| {
                let ok = cron_expr::parse(&task.cron).is_some();
                if !ok {
                    tracing::debug!(id = %task.id, cron = %task.cron, "skipping a scheduled task with an invalid cron");
                }
                ok
            })
            .collect();
        let mut state = self.state();
        for scheduler in state.schedulers.values_mut() {
            scheduler
                .next_fire
                .retain(|id, _| valid.iter().any(|task| &task.id == id));
        }
        state.durable = valid.clone();
        drop(state);
        self.changed();
        valid
    }

    async fn change(&self, change: CronTasksChange) -> Result<(), String> {
        let Some(store) = self.store() else {
            return Ok(());
        };
        let result = store
            .change(change)
            .await
            .map_err(|error| error.to_string());
        self.read_durable().await;
        result
    }

    /// CronCreate `durable: true`；没有存储时返回 false，调用方按会话任务处理。
    pub(crate) async fn add_durable(&self, task: NewCronTask) -> Result<bool, String> {
        if self.store().is_none() {
            return Ok(false);
        }
        self.change(CronTasksChange::Add { task }).await?;
        self.enable();
        Ok(true)
    }

    /// CronDelete：先删会话任务，没有再删文件里的。
    pub(crate) async fn delete(&self, owner: &SessionId, id: &str) -> Result<(), String> {
        if self.delete_session_job(owner, id) {
            return Ok(());
        }
        self.change(CronTasksChange::Remove {
            ids: vec![id.to_owned()],
        })
        .await
    }

    /// 会话的持久调度开始：抢锁 → 订阅监听 → `Claim` → 读文件 → 错过的一次性
    /// 任务删除并排一条提示。返回监听流。
    pub(crate) async fn start_durable(
        &self,
        session: &SessionId,
    ) -> Option<futures_util::stream::BoxStream<'static, ()>> {
        let store = self.store()?;
        let leader = match store.lead(session).await {
            Ok(leader) => leader,
            Err(error) => {
                tracing::warn!(%error, "cannot take the scheduler lock");
                false
            }
        };
        // 先订阅再读：读完之后的写入都会产出变化。
        let changes = store.changes();
        let claim = CronTasksChange::Claim {
            session: session.clone(),
        };
        if let Err(error) = store.change(claim).await {
            tracing::warn!(%error, "cannot refresh the creator of scheduled tasks");
        }
        let tasks = self.read_durable().await;
        let missed = self.mark_missed(session, leader, tasks);
        if !missed.is_empty()
            && let Err(error) = self.change(CronTasksChange::Remove { ids: missed }).await
        {
            tracing::warn!(%error, "cannot remove missed scheduled tasks");
        }
        self.changed();
        Some(changes)
    }

    /// 记下调度状态（持锁与否）；找出错过的一次性任务（属于本会话、没问过），排一条提示，返回它们的 ID。
    fn mark_missed(
        &self,
        session: &SessionId,
        leader: bool,
        tasks: Vec<StoredCronTask>,
    ) -> Vec<String> {
        let now = self.now_ms();
        let offset = self.offset();
        let mut state = self.state();
        let scheduler = state.schedulers.entry(session.clone()).or_default();
        scheduler.started = true;
        scheduler.leader = leader;
        scheduler.next_probe = (!leader).then_some(now + PROBE_INTERVAL_MS);
        let missed: Vec<StoredCronTask> = tasks
            .into_iter()
            .filter(|task| !task.recurring && !scheduler.missed_asked.contains(&task.id))
            .filter(|task| belongs(task, session, leader))
            .filter(|task| {
                cron_expr::next_after(&task.cron, task.created_at, &offset)
                    .is_some_and(|at| at < now)
            })
            .collect();
        for task in &missed {
            scheduler.missed_asked.insert(task.id.clone());
            scheduler.next_fire.insert(task.id.clone(), None);
        }
        if !missed.is_empty() {
            let text = crate::cron_texts::missed(&missed, &offset);
            scheduler.missed_notice = Some((text, now));
        }
        missed.into_iter().map(|task| task.id).collect()
    }

    /// 不持锁的会话到点探测锁；拿到后开始认领无主任务。
    pub(crate) async fn probe(&self, session: &SessionId) {
        let Some(store) = self.store() else {
            return;
        };
        let leader = store.lead(session).await.unwrap_or(false);
        let now = self.now_ms();
        let closed = {
            let mut state = self.state();
            let scheduler = state.schedulers.entry(session.clone()).or_default();
            if !scheduler.closed {
                scheduler.leader = leader;
                scheduler.next_probe = (!leader).then_some(now + PROBE_INTERVAL_MS);
            }
            scheduler.closed
        };
        // 探测的同时会话关了：刚拿到的锁马上放掉。
        if closed && leader {
            let _ = store.release(session).await;
        }
        self.changed();
    }

    /// 会话关闭：停止它的持久调度，释放它的锁。
    pub(crate) async fn close_durable(&self, session: &SessionId) {
        let held = {
            let mut state = self.state();
            let scheduler = state.schedulers.entry(session.clone()).or_default();
            scheduler.closed = true;
            scheduler.next_probe = None;
            std::mem::replace(&mut scheduler.leader, false)
        };
        if held
            && let Some(store) = self.store()
            && let Err(error) = store.release(session).await
        {
            tracing::warn!(%error, "cannot release the scheduler lock");
        }
        self.changed();
    }

    /// 取走到期的持久任务（只在会话空闲时）：循环任务回写 `lastFiredAt`，一次性任务与满 7 天的循环任务删除。
    pub(crate) async fn take_durable(&self, session: &SessionId, now: u64) -> Vec<CronFire> {
        let (fires, fired, removed) = {
            let mut state = self.state();
            due_durable(self, &mut state, session, now)
        };
        if !fired.is_empty() {
            let mark = CronTasksChange::MarkFired {
                ids: fired,
                at: now,
            };
            if let Err(error) = self.change(mark).await {
                tracing::warn!(%error, "cannot persist lastFiredAt");
            }
        }
        if !removed.is_empty() {
            let remove = CronTasksChange::Remove { ids: removed };
            if let Err(error) = self.change(remove).await {
                tracing::warn!(%error, "cannot remove fired scheduled tasks");
            }
        }
        fires
    }
}

/// 持久任务第一次看到时的触发时间：循环任务从 `lastFiredAt ?? createdAt`，一次性任务从 `createdAt`。
pub(crate) fn first_sight(board: &CronBoard, task: &StoredCronTask) -> Option<u64> {
    let from = if task.recurring {
        task.last_fired_at.unwrap_or(task.created_at)
    } else {
        task.created_at
    };
    board.first_fire(&task.cron, task.recurring, &task.id, from)
}

type Due = (Vec<CronFire>, Vec<String>, Vec<String>);

fn due_durable(board: &CronBoard, state: &mut State, session: &SessionId, now: u64) -> Due {
    let mut due = (Vec::new(), Vec::new(), Vec::new());
    let State {
        durable,
        schedulers,
        ..
    } = state;
    let Some(scheduler) = schedulers.get_mut(session) else {
        return due;
    };
    if let Some((text, _)) = scheduler.missed_notice.take() {
        due.0.push(CronFire {
            job: MISSED_JOB.to_owned(),
            prompt: text,
            kind: CronFireKind::Notice,
        });
    }
    if !scheduler.started || scheduler.closed {
        return due;
    }
    let offset = board.offset();
    for task in durable.iter() {
        if !belongs(task, session, scheduler.leader) {
            continue;
        }
        let next = *scheduler
            .next_fire
            .entry(task.id.clone())
            .or_insert_with(|| first_sight(board, task));
        if next.is_none_or(|at| now < at) {
            continue;
        }
        due.0.push(CronFire {
            job: task.id.clone(),
            prompt: task.prompt.clone(),
            kind: CronFireKind::Task,
        });
        let aged = task.recurring
            && !task.permanent
            && now.saturating_sub(task.created_at) >= RECURRING_MAX_AGE_MS;
        if task.recurring && !aged {
            let next = cron_expr::recurring_next(&task.cron, now, &task.id, &offset);
            scheduler.next_fire.insert(task.id.clone(), next);
            due.1.push(task.id.clone());
        } else {
            scheduler.next_fire.insert(task.id.clone(), None);
            due.2.push(task.id.clone());
        }
    }
    due
}

#[cfg(test)]
#[path = "cron_durable_tests.rs"]
mod tests;
