//! Cold load of a Node session (spec rust-m11-node-storage §6): the stored
//! transcript, its active branch, and the model context rebuilt from it.
use super::{messages, sessions};
use anyhow::{Context, Result};
use rusqlite::Connection;
use serde_json::Value;
use zcode_cli_domain::node_history::{self, Branch, Record};

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
        .map(std::borrow::Cow::into_owned)
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

#[cfg(test)]
#[path = "cold_tests.rs"]
mod tests;
