//! Cold load of a Node session (spec rust-m11-node-storage §6): the stored
//! transcript, its active branch, and the model context rebuilt from it.
use super::{entries, messages, sessions, targets};
use anyhow::{Context, Result};
use rusqlite::Connection;
use serde_json::Value;
use std::borrow::Cow;
use zcode_cli_domain::node_history::{self, Branch, Record};
use zcode_cli_domain::node_rows::{self, GoalEntry};

/// The session's messages with parts in storage order.
pub fn records(conn: &Connection, session: &str) -> Result<Vec<Record>> {
    Ok(messages::messages(conn, session)?
        .into_iter()
        .map(|m| Record {
            info: m.info,
            parts: m.parts,
        })
        .collect())
}

/// The rebuilt model context: canonical messages after the last compaction
/// summary, and the summary text itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct History {
    pub summary: Option<String>,
    pub messages: Vec<Value>,
    pub interrupted_tools: usize,
}

/// The active branch of the stored transcript (Node `activeSessionMessages`).
pub fn active(conn: &Connection, session: &str) -> Result<Vec<Record>> {
    let row =
        sessions::get(conn, session)?.with_context(|| format!("Session not found: {session}"))?;
    let branch = Branch::from_revert(row.revert.as_ref());
    let all = records(conn, session)?;
    Ok(node_history::active_messages(&all, &branch, true)
        .into_iter()
        .map(Cow::into_owned)
        .collect())
}

/// The model context of `session` as the live runtime holds it. A leading
/// compaction summary becomes `summary`, like a live compaction.
pub fn history(
    conn: &Connection,
    session: &str,
    artifacts: &dyn Fn(&str) -> Option<String>,
) -> Result<History> {
    let active = active(conn, session)?;
    let summary_record = active
        .first()
        .filter(|r| r.parts.iter().any(node_history::branch::is_boundary_part));
    let summary = summary_record.map(|record| {
        let hydrated = node_history::hydrate(std::slice::from_ref(record), artifacts);
        hydrated
            .entries
            .first()
            .map(|entry| entry.canonical()["content"].clone())
            .and_then(|content| content.as_str().map(str::to_owned))
            .unwrap_or_default()
    });
    let rest = if summary.is_some() {
        &active[1..]
    } else {
        &active[..]
    };
    let hydrated = node_history::hydrate(rest, artifacts);
    Ok(History {
        summary,
        messages: hydrated.entries.iter().map(|e| e.canonical()).collect(),
        interrupted_tools: hydrated.interrupted_tools,
    })
}

/// The persisted facts the cold V4 projection replays (Node
/// `loadPersistedConversationMaterialization`): the active branch, the goal
/// verification entries and the persisted goal.
#[derive(Clone, Debug, PartialEq)]
pub struct Materialization {
    pub messages: Vec<Record>,
    pub goal_entries: Vec<GoalEntry>,
    pub target: Option<Value>,
}

pub fn materialization(conn: &Connection, session: &str) -> Result<Materialization> {
    // Node 读取全部 session_entry 再按 payload 形状挑出 goal 校验事实，不按 type 过滤。
    let stored: Vec<(Value, f64)> = entries::list(conn, session, None)?
        .into_iter()
        .map(|entry| (entry.data, entry.time_created as f64))
        .collect();
    let row =
        sessions::get(conn, session)?.with_context(|| format!("Session not found: {session}"))?;
    let branch = Branch::from_revert(row.revert.as_ref());
    // V4 冷投影只裁掉 rewind 分支（Node selectActiveConversationBranch），不像模型上下文那样
    // 从最后一个压缩边界截断：压缩前的历史仍是可见时间线。
    let all = records(conn, session)?;
    let messages = node_history::select_branch(all.iter().map(Cow::Borrowed).collect(), &branch)
        .into_iter()
        .map(Cow::into_owned)
        .collect();
    Ok(Materialization {
        messages,
        goal_entries: node_rows::goal_entries(&stored),
        target: targets::read(conn, session)?.map(|target| target.to_node()),
    })
}

#[cfg(test)]
#[path = "cold_tests.rs"]
mod tests;
