//! 持久定时任务存储的测试替身：任务放在内存，变化经 watch 通知，锁的另一个持有者由测试控制。

use std::sync::Mutex;

use async_trait::async_trait;
use futures_util::stream::{BoxStream, StreamExt};
use tokio::sync::watch;

use super::lock;
use crate::cron_store::{CronCreator, CronStore, CronTasksChange, StoredCronTask};
use crate::env::EnvError;
use crate::ids::SessionId;

/// 调度锁的持有者。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Holder {
    /// 本进程的某个会话。
    Ours(SessionId),
    /// 别的进程；`alive` 为假时下一次 `lead` 会回收它。
    Foreign { alive: bool },
}

/// 内存里的 [`CronStore`]：`change` 与 `push_external` 都会产出一次变化（等同于文件监听）。
pub struct MemoryCronStore {
    tasks: Mutex<Vec<StoredCronTask>>,
    holder: Mutex<Option<Holder>>,
    version: watch::Sender<u64>,
    changes_made: Mutex<Vec<CronTasksChange>>,
}

impl Default for MemoryCronStore {
    fn default() -> Self {
        Self {
            tasks: Mutex::default(),
            holder: Mutex::default(),
            version: watch::Sender::new(0),
            changes_made: Mutex::default(),
        }
    }
}

impl MemoryCronStore {
    /// 文件里已有这些任务（启动前写好的）。
    pub fn with_tasks(tasks: Vec<StoredCronTask>) -> Self {
        let store = Self::default();
        *lock(&store.tasks) = tasks;
        store
    }

    /// 别的进程写入一项：任务出现在文件里，并产出一次变化。
    pub fn push_external(&self, task: StoredCronTask) {
        lock(&self.tasks).push(task);
        self.version.send_modify(|version| *version += 1);
    }

    /// 别的活着的进程持有调度锁。
    pub fn hold_elsewhere(&self) {
        *lock(&self.holder) = Some(Holder::Foreign { alive: true });
    }

    /// 持有锁的别的进程退出了（没有删锁）。
    pub fn holder_died(&self) {
        let mut holder = lock(&self.holder);
        if let Some(Holder::Foreign { alive }) = holder.as_mut() {
            *alive = false;
        }
    }

    /// 本进程里持有锁的会话；锁空着或在别的进程时为 None。
    pub fn holder(&self) -> Option<SessionId> {
        match lock(&self.holder).clone() {
            Some(Holder::Ours(session)) => Some(session),
            _ => None,
        }
    }

    /// 当前文件里的任务。
    pub fn snapshot(&self) -> Vec<StoredCronTask> {
        lock(&self.tasks).clone()
    }

    /// 收到的读改写，按顺序。
    pub fn changes_made(&self) -> Vec<CronTasksChange> {
        lock(&self.changes_made).clone()
    }
}

#[async_trait]
impl CronStore for MemoryCronStore {
    async fn tasks(&self) -> Result<Vec<StoredCronTask>, EnvError> {
        Ok(self.snapshot())
    }

    async fn change(&self, change: CronTasksChange) -> Result<(), EnvError> {
        lock(&self.changes_made).push(change.clone());
        let mut tasks = lock(&self.tasks);
        match change {
            CronTasksChange::Add { task } => tasks.push(StoredCronTask {
                id: task.id,
                cron: task.cron,
                prompt: task.prompt,
                created_at: task.created_at,
                last_fired_at: None,
                recurring: task.recurring,
                permanent: false,
                created_by: Some(task.created_by),
                creator: CronCreator::ThisProcess,
                in_project: true,
            }),
            CronTasksChange::Remove { ids } => tasks.retain(|task| !ids.contains(&task.id)),
            CronTasksChange::MarkFired { ids, at } => tasks
                .iter_mut()
                .filter(|task| ids.contains(&task.id))
                .for_each(|task| task.last_fired_at = Some(at)),
            CronTasksChange::Claim { session } => tasks
                .iter_mut()
                .filter(|task| task.created_by.as_ref() == Some(&session))
                .for_each(|task| task.creator = CronCreator::ThisProcess),
        }
        drop(tasks);
        self.version.send_modify(|version| *version += 1);
        Ok(())
    }

    fn changes(&self) -> BoxStream<'static, ()> {
        let receiver = self.version.subscribe();
        futures_util::stream::unfold(receiver, |mut receiver| async move {
            receiver.changed().await.ok()?;
            Some(((), receiver))
        })
        .boxed()
    }

    async fn lead(&self, holder: &SessionId) -> Result<bool, EnvError> {
        let mut current = lock(&self.holder);
        let free = match current.as_ref() {
            None | Some(Holder::Foreign { alive: false }) => true,
            Some(Holder::Ours(session)) => session == holder,
            Some(Holder::Foreign { alive: true }) => false,
        };
        if free {
            *current = Some(Holder::Ours(holder.clone()));
        }
        Ok(free)
    }

    async fn release(&self, holder: &SessionId) -> Result<(), EnvError> {
        let mut current = lock(&self.holder);
        if current.as_ref() == Some(&Holder::Ours(holder.clone())) {
            *current = None;
        }
        Ok(())
    }
}
