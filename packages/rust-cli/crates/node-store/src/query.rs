//! 会话管理（修复线 F29，docs/specs/rust-cli/node-store.md §5.1）：← 项目页与 `/resume` 页的查询、改名与删除。
//!
//! 口径照 App 的任务列表（Node `sessions-index`：`task_type in (interactive, fork, workflow_parent)`、本机
//! `workspace_id is null`、没删除，`order by time_updated desc, id desc`），可以不限目录；改名照 Node
//! `setCustomSessionTitle`，删除照 Node `softDeleteSession`。
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::store::{
    SessionOrder, SessionPage, SessionQuery, SessionRecord, StoreError,
};

use crate::repo::sessions;
use crate::{LOCAL_ENVIRONMENT, OrStorage, facts};

/// 首条输入最多保留的字符数（Unicode 标量）。
const FIRST_PROMPT_CHARS: usize = 500;

/// 首条真实用户输入的第一个文字 part：沿 `message_session_time_created_id_idx` 的顺序找第一条
/// `role = user`、`semantics.origin = real_user` 的消息，取它的第一个非合成 text part（按 part 的顺序）。
const FIRST_PROMPT: &str = "select p.data from message m join part p on p.message_id = m.id
      where m.session_id = ?1
        and (case when json_valid(m.data) then json_extract(m.data, '$.role') end) = 'user'
        and (case when json_valid(m.data) then json_extract(m.data, '$.semantics.origin') end) = 'real_user'
        and (case when json_valid(p.data) then json_extract(p.data, '$.type') end) = 'text'
        and coalesce(case when json_valid(p.data) then json_extract(p.data, '$.synthetic') end, 0) = 0
      order by m.time_created, m.id, p.sequence is null, p.sequence, p.time_created, p.id
      limit 1";

fn first_prompt(conn: &Connection, session: &str) -> Result<Option<String>, StoreError> {
    let data: Option<String> = conn
        .prepare_cached(FIRST_PROMPT)
        .storage()?
        .query_row([session], |row| row.get(0))
        .optional()
        .storage()?;
    Ok(data
        .and_then(|data| serde_json::from_str::<serde_json::Value>(&data).ok())
        .and_then(|part| part["text"].as_str().map(str::to_owned))
        .map(|text| text.chars().take(FIRST_PROMPT_CHARS).collect()))
}

pub fn query(conn: &Connection, query: &SessionQuery) -> Result<SessionPage, StoreError> {
    if query.environment.as_str() != LOCAL_ENVIRONMENT {
        return Err(facts::unsupported("sessions of a remote environment"));
    }
    let limit = (query.limit > 0).then(|| i64::try_from(query.limit).unwrap_or(i64::MAX - 1));
    let mut rows = sessions::query(
        conn,
        query.root.as_ref().map(EnvPath::as_str),
        query.order == SessionOrder::Created,
        limit.map(|limit| limit + 1),
    )
    .storage()?;
    let more = limit.is_some_and(|limit| rows.len() as i64 > limit);
    if let Some(limit) = limit {
        rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    }
    let sessions = rows
        .into_iter()
        .map(|row| {
            Ok(SessionRecord {
                first_prompt: first_prompt(conn, &row.id)?,
                id: SessionId::new(row.id),
                title: row.title,
                root: EnvPath::new(row.directory),
                created_at_ms: u64::try_from(row.time_created).unwrap_or(0),
                updated_at_ms: u64::try_from(row.time_updated).unwrap_or(0),
            })
        })
        .collect::<Result<_, StoreError>>()?;
    Ok(SessionPage { sessions, more })
}

/// Node `setCustomSessionTitle`：`updateSession{title, titleSource: custom}`（不推进活动时间）。会话不存在或已删除时
/// `NotFound`。
pub fn rename(
    conn: &mut Connection,
    session: &SessionId,
    title: &str,
    now: u64,
) -> Result<(), StoreError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .storage()?;
    facts::live_session(&tx, session)?;
    let update = sessions::Update {
        id: session.as_str().to_owned(),
        title: Some(title.to_owned()),
        title_source: Some("custom".to_owned()),
        ..Default::default()
    };
    sessions::update(&tx, &update, i64::try_from(now).unwrap_or(i64::MAX)).storage()?;
    tx.commit().storage()
}

/// Node `softDeleteSession`：`time_deleted = coalesce(time_deleted, now)`，消息与活动时间保留。会话不存在时
/// `NotFound`；已删除时不变。
pub fn delete(conn: &mut Connection, session: &SessionId, now: u64) -> Result<(), StoreError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .storage()?;
    if sessions::get(&tx, session.as_str()).storage()?.is_none() {
        return Err(StoreError::NotFound {
            session: session.clone(),
        });
    }
    sessions::soft_delete(
        &tx,
        session.as_str(),
        i64::try_from(now).unwrap_or(i64::MAX),
    )
    .storage()?;
    tx.commit().storage()
}

#[cfg(test)]
#[path = "query_tests.rs"]
mod tests;
