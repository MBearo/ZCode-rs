//! Todo imports use the committed backup, never a newer production snapshot.
use crate::domain::todo::{self, TodoItem};
use anyhow::Result;
use rusqlite::{Connection, OpenFlags, params};
use serde_json::json;
use tokio_util::sync::CancellationToken;

pub(super) fn read(
    snapshot: &Connection,
    id: &str,
    cancel: &CancellationToken,
) -> Result<(Vec<TodoItem>, u64)> {
    super::legacy_attempt::check(cancel)?;
    let exists: bool = snapshot.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='todo')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok((vec![], 0));
    }
    let mut query=snapshot.prepare("SELECT content,status,priority,time_updated FROM todo WHERE session_id=?1 ORDER BY position")?;
    let rows = query.query_map([id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, u64>(3)?,
        ))
    })?;
    let mut todos = vec![];
    let mut updated = 0;
    for row in rows {
        super::legacy_attempt::check(cancel)?;
        let (content, status, priority, time) = row?;
        todos.push(serde_json::from_value(
            json!({"content":content,"status":status,"priority":priority}),
        )?);
        updated = updated.max(time);
    }
    todo::validate(&todos)?;
    Ok((todos, updated))
}

pub(super) fn backfill(
    dest: &mut Connection,
    source: &str,
    workspace: &str,
    cancel: &CancellationToken,
) -> Result<()> {
    let ids = dest
        .prepare(
            "SELECT id FROM rust_session WHERE workspace=?1 AND json_type(body,'$.todos') IS NULL",
        )?
        .query_map([workspace], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if ids.is_empty() {
        return Ok(());
    }
    let backup: String = dest.query_row(
        "SELECT backup FROM rust_legacy_import WHERE source=?1 AND workspace=?2",
        params![source, workspace],
        |r| r.get(0),
    )?;
    let snapshot = Connection::open_with_flags(backup, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let tx = dest.transaction()?;
    for id in ids {
        super::legacy_attempt::check(cancel)?;
        // 旧 Rust 自建会话可能也缺字段；只能补齐同一备份中实际导入的身份。
        let imported: bool=snapshot.query_row("SELECT EXISTS(SELECT 1 FROM session WHERE id=?1 AND COALESCE(NULLIF(TRIM(workspace_id),''),directory)=?2)",params![id,workspace],|r|r.get(0))?;
        if !imported {
            continue;
        }
        let (todos, updated) = read(&snapshot, &id, cancel)?;
        tx.execute("UPDATE rust_session SET body=json_set(body,'$.todos',json(?3),'$.todosUpdatedAt',?4) WHERE workspace=?1 AND id=?2 AND json_type(body,'$.todos') IS NULL",params![workspace,id,serde_json::to_string(&todos)?,updated])?;
    }
    super::legacy_attempt::check(cancel)?;
    tx.commit()?;
    Ok(())
}
