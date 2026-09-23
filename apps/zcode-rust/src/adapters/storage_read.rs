use super::storage::load_items;
use crate::domain::session::Session;
use anyhow::Result;
use rusqlite::Connection;
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
