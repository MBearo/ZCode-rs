use super::storage::load_items;
use crate::domain::session::Session;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use std::collections::BTreeMap;
type StoredWorkspace = (Vec<Session>, BTreeMap<String, Value>);
pub(super) fn load(conn: &Connection, workspace: &str) -> Result<StoredWorkspace> {
    let mut statement = conn.prepare("SELECT body FROM rust_session WHERE workspace=?1")?;
    let mut sessions: Vec<Session> = statement
        .query_map([workspace], |row| row.get::<_, String>(0))?
        .map(|row| Ok(serde_json::from_str(&row?)?))
        .collect::<Result<Vec<_>>>()?;
    for session in &mut sessions {
        // v1 的 body 内嵌完整历史；首次 commit 在同一事务拆分，失败不破坏旧数据。
        if session.rows.is_empty() {
            session.rows = load_items(conn, "rust_row", workspace, &session.id)?;
            session.messages = load_items(conn, "rust_message", workspace, &session.id)?;
            session.saved_rows = session.rows.len();
            session.saved_messages = session.messages.len();
        }
        if session.history.inputs.is_empty() && session.history.responses.is_empty() {
            session.history = super::storage_history::load(conn, workspace, &session.id)?;
            session.saved_inputs = session.history.inputs.len();
            session.saved_responses = session.history.responses.len();
        }
    }
    let mut statement = conn.prepare("SELECT key,ack FROM rust_command WHERE workspace=?1")?;
    let acks = statement
        .query_map([workspace], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .map(|row| {
            let (k, v) = row?;
            Ok((k, serde_json::from_str(&v)?))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    Ok((sessions, acks))
}

pub(super) fn load_session(
    conn: &Connection,
    workspace: &str,
    id: &str,
) -> Result<Option<Session>> {
    let body: Option<String> = conn
        .query_row(
            "SELECT body FROM rust_session WHERE workspace=?1 AND id=?2",
            params![workspace, id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(body) = body else { return Ok(None) };
    let mut session: Session = serde_json::from_str(&body)?;
    if session.rows.is_empty() {
        session.rows = load_items(conn, "rust_row", workspace, id)?;
        session.messages = load_items(conn, "rust_message", workspace, id)?;
        session.saved_rows = session.rows.len();
        session.saved_messages = session.messages.len();
    }
    if session.history.inputs.is_empty() && session.history.responses.is_empty() {
        session.history = super::storage_history::load(conn, workspace, id)?;
        session.saved_inputs = session.history.inputs.len();
        session.saved_responses = session.history.responses.len();
    }
    Ok(Some(session))
}

pub(super) fn discard_draft(
    conn: &mut Connection,
    workspace: &str,
    id: &str,
    ack: Option<(String, Value)>,
) -> Result<()> {
    let tx = conn.transaction()?;
    // 关闭草稿不是真删历史；存储边界再次核查，避免 future caller 用过期 draft 状态误删首发。
    let Some(session) = load_session(&tx, workspace, id)? else {
        // 预热草稿只活在内存；关闭不应为每次界面切换增加永久 ACK 或 WAL 写入。
        return Ok(());
    };
    anyhow::ensure!(
        session.rows.is_empty() && session.messages.is_empty(),
        "Cannot discard a persisted conversation as draft"
    );
    tx.execute(
        "DELETE FROM rust_session WHERE workspace=?1 AND id=?2",
        params![workspace, id],
    )?;
    // session/close 没有命令 ACK；deleteSession 的 ACK 与删除同事务提交。
    if let Some((key, ack)) = ack {
        tx.execute("INSERT INTO rust_command VALUES(?1,?2,?3) ON CONFLICT(workspace,key) DO UPDATE SET ack=excluded.ack", params![workspace,key,serde_json::to_string(&ack)?])?;
    }
    tx.commit()?;
    Ok(())
}
