//! 任务清单（Task v2）的本地存储（docs/specs/rust-cli/agents.md §7.2）。
//!
//! 文件 `<storage.dir>/tasks/<session>.json` 是唯一事实源，经 rust-cli-fs 原子替换；写成功后把清单
//! 投影到 Node 的 `todo` 表供 App 显示（写法同 Node `updateTodos`：`begin immediate`、先删后插、
//! `touchSession`）。投影可以从文件重建，失败不影响文件。
//!
//! 续写 Node 会话（修复线 F2）：还没有清单文件时，从 Node 的 `todo` 表导入（Node `readTodos` 的顺序），第一次
//! 保存时写成文件；Node 的 `priority` 不是占位值时记在任务的 `metadata.priority`，投影时写回。以前只认文件，
//! 看不到 Node 的 todo，第一次改清单还把它们删掉。

use async_trait::async_trait;
use rusqlite::{Connection, params};
use rust_cli_kernel_api::agents::{TaskList, TaskListStore, TaskRecord, TaskStatus};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::store::StoreError;

use crate::{NodeStore, OrStorage, storage};

/// `todo.priority` 的占位（Task v2 没有优先级）。
const PRIORITY: &str = "medium";
/// Node `TodoItemSchema` 的优先级。
const PRIORITIES: [&str; 3] = ["high", "medium", "low"];

/// 投影行：按 ID 的数值升序，`position` 是序号。
fn rows(list: &TaskList) -> Vec<(String, String, String)> {
    let mut tasks: Vec<_> = list.tasks.iter().collect();
    tasks.sort_by_key(|task| task.id.parse::<u64>().unwrap_or(u64::MAX));
    tasks
        .into_iter()
        .map(|task| {
            let priority = task
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("priority"))
                .and_then(serde_json::Value::as_str)
                .filter(|priority| PRIORITIES.contains(priority))
                .unwrap_or(PRIORITY);
            (
                task.subject.clone(),
                task.status.as_str().to_owned(),
                priority.to_owned(),
            )
        })
        .collect()
}

/// Node 写的 todo（`position` 升序）读成任务清单：ID 依次是 1、2…，状态不认识时按 pending。
fn imported(conn: &Connection, session: &str) -> Result<TaskList, StoreError> {
    let mut statement = conn
        .prepare(
            "select content, status, priority from todo where session_id = ? order by position asc",
        )
        .storage()?;
    let todos = statement
        .query_map(params![session], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .storage()?
        .collect::<rusqlite::Result<Vec<_>>>()
        .storage()?;
    let tasks: Vec<TaskRecord> = todos
        .into_iter()
        .enumerate()
        .map(|(index, (subject, status, priority))| TaskRecord {
            id: (index + 1).to_string(),
            subject,
            description: String::new(),
            active_form: None,
            owner: None,
            status: match status.as_str() {
                "in_progress" => TaskStatus::InProgress,
                "completed" => TaskStatus::Completed,
                _ => TaskStatus::Pending,
            },
            blocks: Vec::new(),
            blocked_by: Vec::new(),
            metadata: (priority != PRIORITY)
                .then(|| serde_json::Map::from_iter([("priority".to_owned(), priority.into())])),
        })
        .collect();
    Ok(TaskList {
        high_water_mark: tasks.len() as u64,
        tasks,
    })
}

pub(crate) fn project(
    conn: &mut Connection,
    session: &str,
    rows: &[(String, String, String)],
    now: i64,
) -> Result<(), StoreError> {
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .storage()?;
    tx.execute("delete from todo where session_id = ?", params![session])
        .storage()?;
    {
        let mut insert = tx
            .prepare(
                "insert into todo (session_id, content, status, priority, position, time_created, time_updated) values (?, ?, ?, ?, ?, ?, ?)",
            )
            .storage()?;
        for (position, (content, status, priority)) in rows.iter().enumerate() {
            let position = i64::try_from(position).unwrap_or(i64::MAX);
            insert
                .execute(params![
                    session, content, status, priority, position, now, now
                ])
                .storage()?;
        }
    }
    tx.execute(
        "update session set time_updated = max(time_updated, ?) where id = ?",
        params![now, session],
    )
    .storage()?;
    tx.commit().storage()
}

#[async_trait]
impl TaskListStore for NodeStore {
    async fn load(&self, session: &SessionId) -> Result<TaskList, StoreError> {
        let path = self.paths.task_list(session.as_str());
        match rust_cli_fs::read_optional(path.as_path()).await {
            Ok(Some(bytes)) => serde_json::from_slice(&bytes).map_err(storage),
            Ok(None) => {
                let session = session.as_str().to_owned();
                self.with_db(move |conn, _| imported(conn, &session)).await
            }
            Err(error) => Err(storage(error)),
        }
    }

    async fn save(&self, session: &SessionId, list: &TaskList) -> Result<(), StoreError> {
        let path = self.paths.task_list(session.as_str());
        let mut bytes = serde_json::to_vec_pretty(list).map_err(storage)?;
        bytes.push(b'\n');
        let options = rust_cli_fs::WriteOptions {
            mode: rust_cli_fs::FileMode::Default,
            temp_name: rust_cli_fs::TempName::Hidden,
            parent: rust_cli_fs::ParentDir::Create,
            rename_retry: &[],
        };
        rust_cli_fs::atomic_write(path.as_path(), &bytes, options)
            .await
            .map_err(storage)?;
        let rows = rows(list);
        let session = session.as_str().to_owned();
        self.with_db(move |conn, ids| {
            let now = i64::try_from(ids.now_ms()).unwrap_or(i64::MAX);
            project(conn, &session, &rows, now)
        })
        .await
    }
}

#[cfg(test)]
#[path = "task_lists_tests.rs"]
mod tests;
