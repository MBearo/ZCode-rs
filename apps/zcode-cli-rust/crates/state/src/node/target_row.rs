//! The session goal's `session_target` row as the engine last committed it
//! (spec rust-m11-node-storage §5.4): the final state Node's
//! `session-target.ts` writes leave, applied as one upsert.
use super::sessions::touch;
use super::targets;
use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use serde_json::Value;

/// Upserts the row of Node `SessionGoal` `goal`, or deletes it for `None`.
pub fn put(conn: &Connection, session: &str, goal: Option<&Value>, now: i64) -> Result<()> {
    let Some(goal) = goal else {
        targets::clear(conn, session, now)?;
        return Ok(());
    };
    let text = |key: &str| goal[key].as_str().map(str::to_owned);
    let updated = goal["time"]["updated"].as_i64().context("target time")?;
    conn.prepare_cached(
        "insert into session_target (
      session_id, target_id, objective, summary_title, status, token_budget, tokens_used,
      time_used_seconds, active_input_id, active_run_started_at, active_run_last_seen_at,
      time_created, time_updated
    ) values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    on conflict(session_id) do update set
      target_id = excluded.target_id,
      objective = excluded.objective,
      summary_title = excluded.summary_title,
      status = excluded.status,
      token_budget = excluded.token_budget,
      tokens_used = excluded.tokens_used,
      time_used_seconds = excluded.time_used_seconds,
      active_input_id = excluded.active_input_id,
      active_run_started_at = excluded.active_run_started_at,
      active_run_last_seen_at = excluded.active_run_last_seen_at,
      time_created = excluded.time_created,
      time_updated = excluded.time_updated",
    )?
    .execute(params![
        session,
        text("targetID").context("target id")?,
        text("objective").context("target objective")?,
        text("summaryTitle"),
        text("status").context("target status")?,
        goal["tokenBudget"].as_i64(),
        goal["tokensUsed"].as_i64().unwrap_or(0),
        goal["timeUsedSeconds"].as_i64().unwrap_or(0),
        text("activeInputId"),
        goal["activeRunStartedAtMs"].as_i64(),
        goal["activeRunLastSeenAtMs"].as_i64(),
        goal["time"]["created"].as_i64().context("target time")?,
        updated,
    ])?;
    touch(conn, session, updated)?;
    Ok(())
}
