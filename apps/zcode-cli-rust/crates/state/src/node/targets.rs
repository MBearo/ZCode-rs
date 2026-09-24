//! `session_target` (goal) rows (Node `adapters/src/storage/session-target.ts`).
//! Every change touches the session. Spec rust-m11-node-storage §4.
use super::sessions::touch;
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, Row, params};

/// Node `SessionGoal`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub session_id: String,
    pub target_id: String,
    pub objective: String,
    pub summary_title: Option<String>,
    pub status: String,
    pub token_budget: Option<i64>,
    pub tokens_used: i64,
    pub time_used_seconds: i64,
    pub active_input_id: Option<String>,
    pub active_run_started_at: Option<i64>,
    pub active_run_last_seen_at: Option<i64>,
    pub time_created: i64,
    pub time_updated: i64,
}

impl Target {
    /// The Node `SessionGoal` object (`decodeTargetRow`).
    pub fn to_node(&self) -> serde_json::Value {
        serde_json::json!({
            "sessionID": self.session_id,
            "targetID": self.target_id,
            "objective": self.objective,
            "summaryTitle": self.summary_title,
            "status": self.status,
            "tokenBudget": self.token_budget,
            "tokensUsed": self.tokens_used,
            "timeUsedSeconds": self.time_used_seconds,
            "activeInputId": self.active_input_id,
            "activeRunStartedAtMs": self.active_run_started_at,
            "activeRunLastSeenAtMs": self.active_run_last_seen_at,
            "time": {"created": self.time_created, "updated": self.time_updated},
        })
    }
}

fn decode(row: &Row) -> rusqlite::Result<Target> {
    Ok(Target {
        session_id: row.get("session_id")?,
        target_id: row.get("target_id")?,
        objective: row.get("objective")?,
        summary_title: row.get("summary_title")?,
        status: row.get("status")?,
        token_budget: row.get("token_budget")?,
        tokens_used: row.get("tokens_used")?,
        time_used_seconds: row.get("time_used_seconds")?,
        active_input_id: row.get("active_input_id")?,
        active_run_started_at: row.get("active_run_started_at")?,
        active_run_last_seen_at: row.get("active_run_last_seen_at")?,
        time_created: row.get("time_created")?,
        time_updated: row.get("time_updated")?,
    })
}

/// Node `readSessionTarget`.
pub fn read(conn: &Connection, session: &str) -> Result<Option<Target>> {
    Ok(conn
        .prepare_cached("select * from session_target where session_id = ?")?
        .query_row([session], decode)
        .optional()?)
}

fn must_read(conn: &Connection, session: &str) -> Result<Target> {
    read(conn, session)?.with_context(|| format!("Session target not found after write: {session}"))
}

/// Node `elapsedSecondsBetween`.
fn elapsed_seconds(started: i64, ended: i64) -> i64 {
    ((ended - started).max(0) + 999) / 1000
}

/// Node `setSessionTarget`: a fresh target replacing any previous one.
pub fn set(
    conn: &Connection,
    session: &str,
    target_id: &str,
    objective: &str,
    status: &str,
    token_budget: Option<i64>,
    now: i64,
) -> Result<Target> {
    conn.prepare_cached(
        "insert into session_target (
      session_id, target_id, objective, summary_title, status, token_budget, tokens_used, time_used_seconds, time_created, time_updated
    ) values (?, ?, ?, null, ?, ?, 0, 0, ?, ?)
    on conflict(session_id) do update set
      target_id = excluded.target_id,
      objective = excluded.objective,
      summary_title = excluded.summary_title,
      status = excluded.status,
      token_budget = excluded.token_budget,
      tokens_used = excluded.tokens_used,
      time_used_seconds = excluded.time_used_seconds,
      active_input_id = null,
      active_run_started_at = null,
      active_run_last_seen_at = null,
      time_created = excluded.time_created,
      time_updated = excluded.time_updated",
    )?
    .execute(params![session, target_id, objective, status, token_budget, now, now])?;
    touch(conn, session, now)?;
    must_read(conn, session)
}

/// Node `cloneSessionTargetForFork`: keeps the source identity and times,
/// never the parent's active run.
pub fn clone_for_fork(
    conn: &Connection,
    session: &str,
    source: &Target,
    status: &str,
    now: i64,
) -> Result<Target> {
    conn.prepare_cached(
        "insert into session_target (
      session_id, target_id, objective, summary_title, status, token_budget, tokens_used, time_used_seconds,
      active_input_id, active_run_started_at, active_run_last_seen_at, time_created, time_updated
    ) values (?, ?, ?, ?, ?, ?, ?, ?, null, null, null, ?, ?)
    on conflict(session_id) do update set
      target_id = excluded.target_id,
      objective = excluded.objective,
      summary_title = excluded.summary_title,
      status = excluded.status,
      token_budget = excluded.token_budget,
      tokens_used = excluded.tokens_used,
      time_used_seconds = excluded.time_used_seconds,
      active_input_id = null,
      active_run_started_at = null,
      active_run_last_seen_at = null,
      time_created = excluded.time_created,
      time_updated = excluded.time_updated",
    )?
    .execute(params![
        session,
        source.target_id,
        source.objective,
        source.summary_title,
        status,
        source.token_budget,
        source.tokens_used,
        source.time_used_seconds,
        source.time_created,
        source.time_updated
    ])?;
    touch(conn, session, now)?;
    must_read(conn, session)
}

/// Node `updateSessionTargetStatus`.
pub fn update_status(
    conn: &Connection,
    session: &str,
    status: &str,
    now: i64,
) -> Result<Option<Target>> {
    let changed = conn
        .prepare_cached(
            "update session_target set status = ?, time_updated = ? where session_id = ?",
        )?
        .execute(params![status, now, session])?;
    if changed == 0 {
        return Ok(None);
    }
    touch(conn, session, now)?;
    must_read(conn, session).map(Some)
}

/// Node `startSessionTargetRun` (only an `active` target of that id).
pub fn start_run(
    conn: &Connection,
    session: &str,
    target_id: &str,
    input_id: &str,
    started_at: i64,
) -> Result<Option<Target>> {
    let started = started_at.max(0);
    let changed = conn
        .prepare_cached(
            "update session_target set
        active_input_id = ?, active_run_started_at = ?, active_run_last_seen_at = ?,
        time_updated = max(time_updated, ?)
      where session_id = ? and target_id = ? and status = 'active'",
        )?
        .execute(params![
            input_id, started, started, started, session, target_id
        ])?;
    if changed == 0 {
        return read(conn, session);
    }
    touch(conn, session, started)?;
    must_read(conn, session).map(Some)
}

/// Node `heartbeatSessionTargetRun`.
pub fn heartbeat_run(
    conn: &Connection,
    session: &str,
    target_id: &str,
    input_id: &str,
    seen_at: i64,
) -> Result<Option<Target>> {
    let seen = seen_at.max(0);
    let changed = conn
        .prepare_cached(
            "update session_target set
        active_run_last_seen_at = max(coalesce(active_run_last_seen_at, 0), ?),
        time_updated = max(time_updated, ?)
      where session_id = ? and target_id = ? and active_input_id = ? and active_run_started_at is not null",
        )?
        .execute(params![seen, seen, session, target_id, input_id])?;
    if changed == 0 {
        return read(conn, session);
    }
    touch(conn, session, seen)?;
    must_read(conn, session).map(Some)
}

/// Node `finishSessionTargetRun`: a CAS on the active run that accounts time
/// and tokens; an explicit status wins, else the budget may limit the goal.
pub fn finish_run(
    conn: &Connection,
    session: &str,
    target_id: &str,
    input_id: &str,
    ended_at: i64,
    status: Option<&str>,
    tokens_delta: i64,
) -> Result<Option<Target>> {
    let current = read(conn, session)?;
    let Some(started) = current
        .as_ref()
        .filter(|t| t.target_id == target_id && t.active_input_id.as_deref() == Some(input_id))
        .and_then(|t| t.active_run_started_at)
    else {
        return Ok(current);
    };
    let ended = ended_at.max(0);
    let tokens = tokens_delta.max(0);
    let changed = conn
        .prepare_cached(
            "update session_target set
        tokens_used = tokens_used + ?,
        time_used_seconds = time_used_seconds + ?,
        status = case
          when ? is not null then ?
          when status = 'active' and token_budget is not null and tokens_used + ? >= token_budget then 'budget_limited'
          else status
        end,
        active_input_id = null,
        active_run_started_at = null,
        active_run_last_seen_at = null,
        time_updated = max(time_updated, ?)
      where session_id = ? and target_id = ? and active_input_id = ? and active_run_started_at = ?",
        )?
        .execute(params![
            tokens,
            elapsed_seconds(started, ended),
            status,
            status,
            tokens,
            ended,
            session,
            target_id,
            input_id,
            started
        ])?;
    if changed == 0 {
        return read(conn, session);
    }
    touch(conn, session, ended)?;
    must_read(conn, session).map(Some)
}

/// Node `recoverInterruptedSessionTargetRun`: settles a stale run up to its
/// last heartbeat (never the offline time); an active goal pauses.
pub fn recover_interrupted_run(conn: &Connection, session: &str) -> Result<Option<Target>> {
    let current = read(conn, session)?;
    let Some(target) = current.as_ref() else {
        return Ok(None);
    };
    let (Some(input), Some(started)) = (
        target.active_input_id.as_deref(),
        target.active_run_started_at,
    ) else {
        return Ok(current);
    };
    let ended = target.active_run_last_seen_at.unwrap_or(started);
    let status = if target.status == "active" {
        "paused"
    } else {
        target.status.as_str()
    };
    let changed = conn
        .prepare_cached(
            "update session_target set
        time_used_seconds = time_used_seconds + ?,
        status = ?,
        active_input_id = null,
        active_run_started_at = null,
        active_run_last_seen_at = null,
        time_updated = max(time_updated, ?)
      where session_id = ? and target_id = ? and active_input_id = ? and active_run_started_at = ?",
        )?
        .execute(params![
            elapsed_seconds(started, ended),
            status,
            ended,
            session,
            target.target_id,
            input,
            started
        ])?;
    if changed == 0 {
        return read(conn, session);
    }
    touch(conn, session, ended)?;
    must_read(conn, session).map(Some)
}

/// Node `accountSessionTargetUsage`.
pub fn account_usage(
    conn: &Connection,
    session: &str,
    target_id: &str,
    tokens_delta: i64,
    seconds_delta: i64,
    now: i64,
) -> Result<Option<Target>> {
    let (tokens, seconds) = (tokens_delta.max(0), seconds_delta.max(0));
    if tokens == 0 && seconds == 0 {
        return read(conn, session);
    }
    let changed = conn
        .prepare_cached(
            "update session_target set
        tokens_used = tokens_used + ?,
        time_used_seconds = time_used_seconds + ?,
        status = case
          when status = 'active' and token_budget is not null and tokens_used + ? >= token_budget then 'budget_limited'
          else status
        end,
        time_updated = ?
      where session_id = ? and target_id = ?",
        )?
        .execute(params![tokens, seconds, tokens, now, session, target_id])?;
    if changed == 0 {
        return read(conn, session);
    }
    touch(conn, session, now)?;
    must_read(conn, session).map(Some)
}

/// Node `updateSessionTargetSummaryTitle` (CAS on the target id).
pub fn update_summary_title(
    conn: &Connection,
    session: &str,
    target_id: &str,
    title: &str,
    now: i64,
) -> Result<Option<Target>> {
    let changed = conn
        .prepare_cached(
            "update session_target set summary_title = ?, time_updated = ? where session_id = ? and target_id = ?",
        )?
        .execute(params![title, now, session, target_id])?;
    if changed == 0 {
        return read(conn, session);
    }
    touch(conn, session, now)?;
    must_read(conn, session).map(Some)
}

/// Node `clearSessionTarget`.
pub fn clear(conn: &Connection, session: &str, now: i64) -> Result<bool> {
    let changed = conn
        .prepare_cached("delete from session_target where session_id = ?")?
        .execute([session])?;
    if changed == 0 {
        return Ok(false);
    }
    touch(conn, session, now)?;
    Ok(true)
}
