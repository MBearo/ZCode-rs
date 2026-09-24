//! The stored facts of Node `listSessionSubagents` (spec
//! rust-m11-node-storage §6.3): the parent row and transcript, and the
//! transcripts of its subagent child sessions.
use super::{cold, sessions};
use crate::domain::subagent_query::{self, Live, StoredChild, StoredFacts};
use anyhow::Result;
use rusqlite::Connection;

/// `None` when the parent has no session row (Node: `Session not found`).
pub fn facts(conn: &Connection, parent: &str, live: &Live) -> Result<Option<StoredFacts>> {
    let Some(row) = sessions::get(conn, parent)? else {
        return Ok(None);
    };
    let messages = cold::records(conn, parent)?;
    let mut children = vec![];
    for id in subagent_query::child_session_ids(&messages, row.revert.as_ref(), live) {
        // Node 只读取 subagent_child 类型子会话的消息。
        let Some(child) = sessions::get(conn, &id)?.filter(|c| c.task_type == "subagent_child")
        else {
            continue;
        };
        let stored = StoredChild {
            task_type: child.task_type,
            updated: child.time_updated.max(0) as u64,
            messages: cold::records(conn, &id)?,
        };
        children.push((id, stored));
    }
    Ok(Some(StoredFacts {
        updated: row.time_updated.max(0) as u64,
        revert: row.revert,
        messages,
        children,
    }))
}
