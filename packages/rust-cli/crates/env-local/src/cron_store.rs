//! 持久定时任务存储的本地实现（docs/specs/rust-cli/tools-more.md §16.2）：`<项目根>/.zcode/` 下的
//! `scheduled_tasks.json`（读、读改写、监听）与 `scheduled_tasks.lock`（调度锁）。
//!
//! 创建者进程的 PID 与启动时间只在这里出现：写入时补上本进程的，读出时换成判定结果（[`CronCreator`]），
//! 判定规则见 tools-more.md §16.2（本进程建的、创建者已退出）。

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::cron_store::{CronCreator, CronStore, CronTasksChange, StoredCronTask};
use rust_cli_kernel_api::env::{EnvError, EnvPath};
use rust_cli_kernel_api::ids::SessionId;
use tokio::sync::OnceCell;

use crate::cron_file::{self, FileTask};

/// 定时任务的文件在 `<项目>/.zcode/` 下（名字取 base，F55；cron_watch 按同一个文件名过滤事件）。
const DIR: &str = rust_cli_base::names::DOT_DIR;
pub(crate) const TASKS_FILE: &str = rust_cli_base::names::SCHEDULED_TASKS_FILE;
const LOCK_FILE: &str = "scheduled_tasks.lock";

/// 本进程的身份（锁文件与任务的 `createdByPid` / `createdByProcStart`）。
pub(crate) struct Identity {
    pub(crate) pid: u32,
    pub(crate) started: Option<String>,
}

pub(crate) fn write_options() -> rust_cli_fs::WriteOptions {
    rust_cli_fs::WriteOptions {
        mode: rust_cli_fs::FileMode::Default,
        temp_name: rust_cli_fs::TempName::Hidden,
        parent: rust_cli_fs::ParentDir::Create,
        rename_retry: &[],
    }
}

fn env_error(error: &io::Error) -> EnvError {
    EnvError::Io {
        message: error.to_string(),
    }
}

/// 路径的比较键：统一斜杠、去掉末尾斜杠；macOS 与 Windows 不分大小写。
fn project_key(path: &str) -> String {
    let unified = path.replace('\\', "/");
    let trimmed = match unified.trim_end_matches('/') {
        "" => "/",
        rest => rest,
    };
    if cfg!(any(target_os = "macos", windows)) {
        trimmed.to_lowercase()
    } else {
        trimmed.to_owned()
    }
}

/// 本地的 [`CronStore`]：一个项目根一个实例。
pub struct LocalCronStore {
    dir: PathBuf,
    /// 写进 `createdInProject` 的项目根原文。
    project: String,
    /// 本进程开始的时间：没有启动时间的旧项按它判断是不是本进程建的。
    started_ms: u64,
    identity: OnceCell<Identity>,
    /// 入口注入的时钟（F7，审查 R2-11：以前直接读系统时间）。
    clock: Arc<dyn Clock>,
}

impl LocalCronStore {
    pub fn new(root: &EnvPath, clock: Arc<dyn Clock>) -> Self {
        Self {
            dir: PathBuf::from(root.as_str()).join(DIR),
            project: root.as_str().to_owned(),
            started_ms: clock.now_ms(),
            identity: OnceCell::new(),
            clock,
        }
    }

    async fn identity(&self) -> &Identity {
        self.identity
            .get_or_init(|| async {
                let pid = std::process::id();
                Identity {
                    pid,
                    started: rust_cli_proc::process_started(pid).await,
                }
            })
            .await
    }

    async fn read(&self) -> Result<Vec<FileTask>, EnvError> {
        let path = self.dir.join(TASKS_FILE);
        let bytes = rust_cli_fs::read_optional(&path)
            .await
            .map_err(|error| env_error(&error))?;
        Ok(bytes.map_or_else(Vec::new, |bytes| {
            cron_file::parse(&String::from_utf8_lossy(&bytes))
        }))
    }

    async fn write(&self, tasks: &[FileTask]) -> Result<(), EnvError> {
        let path = self.dir.join(TASKS_FILE);
        rust_cli_fs::atomic_write(&path, cron_file::render(tasks).as_bytes(), write_options())
            .await
            .map_err(|error| env_error(&error))
    }

    /// “本进程建的”与“创建者已退出”的判断；`starts` 是这一次读里查过的启动时间。
    async fn creator(
        &self,
        task: &FileTask,
        me: &Identity,
        starts: &mut HashMap<u32, Option<String>>,
    ) -> CronCreator {
        let Some(pid) = task.created_by_pid else {
            return CronCreator::Gone;
        };
        if pid == me.pid {
            return match (&task.created_by_proc_start, &me.started) {
                (Some(recorded), Some(own)) if recorded == own => CronCreator::ThisProcess,
                // PID 被本进程复用：原来的创建者已经不在。
                (Some(_), Some(_)) => CronCreator::Gone,
                (Some(_), None) => CronCreator::Running,
                (None, _) if task.created_at >= self.started_ms => CronCreator::ThisProcess,
                (None, _) => CronCreator::Running,
            };
        }
        if !rust_cli_fs::process_alive(pid) {
            return CronCreator::Gone;
        }
        let Some(recorded) = &task.created_by_proc_start else {
            return CronCreator::Running;
        };
        let current = match starts.get(&pid) {
            Some(current) => current.clone(),
            None => {
                let current = rust_cli_proc::process_started(pid).await;
                starts.insert(pid, current.clone());
                current
            }
        };
        match current {
            Some(current) if &current != recorded => CronCreator::Gone,
            _ => CronCreator::Running,
        }
    }
}

#[async_trait]
impl CronStore for LocalCronStore {
    async fn tasks(&self) -> Result<Vec<StoredCronTask>, EnvError> {
        let tasks = self.read().await?;
        let me = self.identity().await;
        let project = project_key(&self.project);
        let mut starts = HashMap::new();
        let mut stored = Vec::with_capacity(tasks.len());
        for task in tasks {
            let creator = self.creator(&task, me, &mut starts).await;
            stored.push(StoredCronTask {
                in_project: task
                    .created_in_project
                    .as_deref()
                    .is_some_and(|path| project_key(path) == project),
                created_by: task.created_by_session.map(SessionId::new),
                creator,
                id: task.id,
                cron: task.cron,
                prompt: task.prompt,
                created_at: task.created_at,
                last_fired_at: task.last_fired_at,
                recurring: task.recurring,
                permanent: task.permanent,
            });
        }
        Ok(stored)
    }

    async fn change(&self, change: CronTasksChange) -> Result<(), EnvError> {
        let mut tasks = self.read().await?;
        let me = self.identity().await;
        let changed = match change {
            CronTasksChange::Add { task } => {
                tasks.push(FileTask {
                    id: task.id,
                    cron: task.cron,
                    prompt: task.prompt,
                    created_at: task.created_at,
                    last_fired_at: None,
                    recurring: task.recurring,
                    permanent: false,
                    created_by_session: Some(task.created_by.as_str().to_owned()),
                    created_by_pid: Some(me.pid),
                    created_by_proc_start: me.started.clone(),
                    created_in_project: Some(self.project.clone()),
                });
                true
            }
            CronTasksChange::Remove { ids } => {
                let before = tasks.len();
                tasks.retain(|task| !ids.contains(&task.id));
                tasks.len() != before
            }
            CronTasksChange::MarkFired { ids, at } => {
                let mut changed = false;
                for task in tasks.iter_mut().filter(|task| ids.contains(&task.id)) {
                    task.last_fired_at = Some(at);
                    changed = true;
                }
                changed
            }
            CronTasksChange::Claim { session } => {
                let mut changed = false;
                for task in tasks.iter_mut().filter(|task| {
                    task.created_by_session.as_deref() == Some(session.as_str())
                        && task.created_by_pid != Some(me.pid)
                }) {
                    task.created_by_pid = Some(me.pid);
                    task.created_by_proc_start = me.started.clone();
                    changed = true;
                }
                changed
            }
        };
        if changed {
            self.write(&tasks).await?;
        }
        Ok(())
    }

    fn changes(&self) -> BoxStream<'static, ()> {
        crate::cron_watch::changes(self.dir.clone())
    }

    async fn lead(&self, holder: &SessionId) -> Result<bool, EnvError> {
        let me = self.identity().await;
        crate::cron_lock::acquire(
            &self.dir.join(LOCK_FILE),
            holder.as_str(),
            me,
            self.clock.now_ms(),
        )
        .await
        .map_err(|error| env_error(&error))
    }

    async fn release(&self, holder: &SessionId) -> Result<(), EnvError> {
        crate::cron_lock::release(&self.dir.join(LOCK_FILE), holder.as_str())
            .await
            .map_err(|error| env_error(&error))
    }
}

#[cfg(test)]
#[path = "cron_store_tests.rs"]
mod tests;
