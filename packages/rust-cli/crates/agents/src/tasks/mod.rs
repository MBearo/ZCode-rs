//! 任务清单 Task v2（docs/specs/rust-cli/agents.md §7）。
//!
//! 清单属于顶层会话；文件（[`TaskListStore`]）是唯一事实源。读改写都交给 [`TaskBoard`] 的 owner 任务
//! 按顺序处理（本进程是清单唯一的写者），写成功后经信号把新清单交给顶层会话的 actor（`TasksChanged`）。

mod edit;
mod tools;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use rust_cli_base::Spawner;
use tokio::sync::{mpsc, oneshot};

use async_trait::async_trait;
use rust_cli_kernel_api::agents::{TaskList, TaskListStore};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::store::StoreError;
pub(crate) use tools::tools;

/// 这些旧模型默认打开任务工具。
const LEGACY_MODELS: &[&str] = &[
    "claude-3-opus",
    "claude-3-sonnet",
    "claude-3-haiku",
    "claude-3-5-sonnet",
    "claude-3-5-haiku",
    "claude-3-7-sonnet",
    "claude-opus-4-0",
    "claude-opus-4-1",
    "claude-opus-4-5",
    "claude-opus-4-6",
    "claude-opus-4-7",
    "claude-sonnet-4-0",
    "claude-sonnet-4-5",
    "claude-sonnet-4-6",
    "claude-haiku-4-5",
];

/// 模型没有模型卡片、是上面的旧模型，或显式打开时启用。
pub fn task_tools_enabled(model: &str, opt_in: bool) -> bool {
    let canonical = model.split_once('[').map_or(model, |(id, _)| id);
    opt_in
        || rust_cli_prompts::models::card(canonical).is_none()
        || LEGACY_MODELS
            .iter()
            .any(|legacy| canonical.starts_with(legacy))
}

/// 没有本地存储时（测试、嵌入）用的内存清单。
#[derive(Default)]
pub struct MemoryTaskLists(Mutex<HashMap<SessionId, TaskList>>);

#[async_trait]
impl TaskListStore for MemoryTaskLists {
    async fn load(&self, session: &SessionId) -> Result<TaskList, StoreError> {
        let lists = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(lists.get(session).cloned().unwrap_or_default())
    }

    async fn save(&self, session: &SessionId, list: &TaskList) -> Result<(), StoreError> {
        let mut lists = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        lists.insert(session.clone(), list.clone());
        Ok(())
    }
}

/// 一次修改：返回给模型的文本与“是否改了”。
pub(crate) type Change = Box<dyn FnOnce(&mut TaskList) -> (String, bool) + Send>;

enum Job {
    Read {
        list: SessionId,
        reply: oneshot::Sender<Result<TaskList, StoreError>>,
    },
    Update {
        list: SessionId,
        change: Change,
        reply: oneshot::Sender<Result<(String, Option<TaskList>), StoreError>>,
    },
}

/// 任务清单的 owner：一个任务按顺序处理全部读改写，同一份清单不会交错修改（不靠跨 await 的锁）。
pub(crate) struct TaskBoard {
    jobs: mpsc::Sender<Job>,
}

fn closed() -> StoreError {
    StoreError::Storage {
        message: "the task board is closed".to_owned(),
    }
}

async fn serve(store: Arc<dyn TaskListStore>, mut jobs: mpsc::Receiver<Job>) {
    while let Some(job) = jobs.recv().await {
        match job {
            Job::Read { list, reply } => {
                let _ = reply.send(store.load(&list).await);
            }
            Job::Update {
                list,
                change,
                reply,
            } => {
                let result = match store.load(&list).await {
                    Ok(mut tasks) => {
                        let (text, changed) = change(&mut tasks);
                        if changed {
                            store
                                .save(&list, &tasks)
                                .await
                                .map(|()| (text, Some(tasks)))
                        } else {
                            Ok((text, None))
                        }
                    }
                    Err(error) => Err(error),
                };
                let _ = reply.send(result);
            }
        }
    }
}

impl TaskBoard {
    pub(crate) fn new(store: Arc<dyn TaskListStore>, spawner: &Spawner) -> Self {
        let (jobs, receiver) = mpsc::channel(64);
        let _task = spawner.spawn_resident("task-board", serve(store, receiver));
        Self { jobs }
    }

    pub(crate) async fn read(&self, list: &SessionId) -> Result<TaskList, StoreError> {
        let (reply, answer) = oneshot::channel();
        let job = Job::Read {
            list: list.clone(),
            reply,
        };
        self.jobs.send(job).await.map_err(|_| closed())?;
        answer.await.map_err(|_| closed())?
    }

    /// 读、改、写；没改时不写文件。返回文本与写后的清单（没改时为 None）。
    pub(crate) async fn update(
        &self,
        list: &SessionId,
        change: Change,
    ) -> Result<(String, Option<TaskList>), StoreError> {
        let (reply, answer) = oneshot::channel();
        let job = Job::Update {
            list: list.clone(),
            change,
            reply,
        };
        self.jobs.send(job).await.map_err(|_| closed())?;
        answer.await.map_err(|_| closed())?
    }
}

#[cfg(test)]
#[path = "tasks_tests.rs"]
mod tests;
