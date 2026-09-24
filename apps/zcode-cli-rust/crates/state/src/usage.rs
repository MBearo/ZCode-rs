//! Usage tables (Node migration `0010_usage_observability`) and their writes,
//! run on the storage worker. Spec rust-m9-usage-logs §2.2.
use crate::domain::usage::{Fact, ModelFact, RETENTION_MS, ToolFact, TurnFact};
use anyhow::Result;
use rusqlite::{Connection, params};
use std::time::{Duration, Instant};

/// Pruning runs at most this often (Node prunes after every write).
const PRUNE_INTERVAL: Duration = Duration::from_secs(60);

pub(super) const TABLES: [&str; 3] = ["model_usage", "turn_usage", "tool_usage"];

fn model(conn: &Connection, prefix: &str, f: &ModelFact) -> Result<()> {
    let raw = f
        .raw_usage
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    // Node 的冲突更新覆盖全部列，与整行替换等价。
    conn.prepare_cached(
        &"INSERT OR REPLACE INTO rust_model_usage(id,logical_request_id,attempt_index,session_id,turn_id,
          trace_id,query_source,provider_id,model_id,variant,agent,mode,task_type,status,started_at,
          first_token_at,completed_at,duration_ms,time_to_first_token_ms,finish_reason,tool_call_count,
          input_tokens,output_tokens,reasoning_tokens,cache_creation_input_tokens,cache_read_input_tokens,
          provider_total_tokens,computed_total_tokens,retry_count,retryable,cancelled_by_user,
          context_exceeded,error_type,error_code,error_message,raw_usage_json)
        VALUES(?1,?2,0,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,
          ?25,?26,?27,?28,?29,?30,?31,?32,?33,?34,?35)"
            .replace("rust_", prefix),
    )?
    .execute(params![
        f.id,
        f.logical_request_id,
        f.session_id,
        f.turn_id,
        f.trace_id,
        f.query_source,
        f.provider_id,
        f.model_id,
        f.variant,
        f.agent,
        f.mode,
        f.task_type,
        f.status,
        f.started_at as i64,
        f.first_token_at.map(|at| at as i64),
        f.completed_at as i64,
        f.completed_at.saturating_sub(f.started_at) as i64,
        f.first_token_at.map(|at| at.saturating_sub(f.started_at) as i64),
        f.finish_reason,
        f.tool_call_count as i64,
        f.tokens.input as i64,
        f.tokens.output as i64,
        f.tokens.reasoning as i64,
        f.tokens.cache_write as i64,
        f.tokens.cache_read as i64,
        f.provider_total_tokens.map(|n| n as i64),
        f.tokens.computed_total() as i64,
        f.retry_count as i64,
        f.retryable,
        f.status == "cancelled",
        f.context_exceeded,
        f.error.kind,
        f.error.code,
        f.error.message,
        raw,
    ])?;
    Ok(())
}

fn turn(conn: &Connection, prefix: &str, f: &TurnFact) -> Result<()> {
    conn.prepare_cached(
        &"INSERT INTO rust_turn_usage(session_id,turn_id,trace_id,status,started_at,first_model_start_at,
          first_token_at,completed_at,duration_ms,time_to_first_token_ms,model_request_count,
          model_retry_count,tool_call_count,tool_error_count,input_tokens,output_tokens,reasoning_tokens,
          cache_creation_input_tokens,cache_read_input_tokens,computed_total_tokens,retryable,
          cancelled_by_user,context_exceeded,error_type,error_code)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,0,?21,?22,?23,?24)
        ON CONFLICT(session_id,turn_id) DO UPDATE SET
          trace_id=coalesce(excluded.trace_id,rust_turn_usage.trace_id),
          status=excluded.status,
          started_at=min(rust_turn_usage.started_at,excluded.started_at),
          first_model_start_at=coalesce(rust_turn_usage.first_model_start_at,excluded.first_model_start_at),
          first_token_at=coalesce(rust_turn_usage.first_token_at,excluded.first_token_at),
          completed_at=coalesce(excluded.completed_at,rust_turn_usage.completed_at),
          duration_ms=coalesce(excluded.duration_ms,rust_turn_usage.duration_ms),
          time_to_first_token_ms=coalesce(excluded.time_to_first_token_ms,rust_turn_usage.time_to_first_token_ms),
          model_request_count=excluded.model_request_count,model_retry_count=excluded.model_retry_count,
          tool_call_count=excluded.tool_call_count,tool_error_count=excluded.tool_error_count,
          input_tokens=excluded.input_tokens,output_tokens=excluded.output_tokens,
          reasoning_tokens=excluded.reasoning_tokens,
          cache_creation_input_tokens=excluded.cache_creation_input_tokens,
          cache_read_input_tokens=excluded.cache_read_input_tokens,
          computed_total_tokens=excluded.computed_total_tokens,retryable=excluded.retryable,
          cancelled_by_user=excluded.cancelled_by_user,context_exceeded=excluded.context_exceeded,
          error_type=coalesce(excluded.error_type,rust_turn_usage.error_type),
          error_code=coalesce(excluded.error_code,rust_turn_usage.error_code)"
            .replace("rust_", prefix),
    )?
    .execute(params![
        f.session_id,
        f.turn_id,
        f.trace_id,
        f.status,
        f.started_at as i64,
        f.first_model_start_at.map(|at| at as i64),
        f.first_token_at.map(|at| at as i64),
        f.completed_at as i64,
        f.completed_at.saturating_sub(f.started_at) as i64,
        f.first_token_at.map(|at| at.saturating_sub(f.started_at) as i64),
        f.model_request_count as i64,
        f.model_retry_count as i64,
        f.tool_call_count as i64,
        f.tool_error_count as i64,
        f.tokens.input as i64,
        f.tokens.output as i64,
        f.tokens.reasoning as i64,
        f.tokens.cache_write as i64,
        f.tokens.cache_read as i64,
        f.computed_total_tokens as i64,
        f.cancelled_by_user,
        f.context_exceeded,
        f.error.kind,
        f.error.code,
    ])?;
    Ok(())
}

fn tool(conn: &Connection, prefix: &str, f: &ToolFact) -> Result<()> {
    conn.prepare_cached(
        &"INSERT INTO rust_tool_usage(id,session_id,turn_id,trace_id,tool_call_id,tool_name,approval_status,
          status,started_at,completed_at,duration_ms,output_bytes,cancelled_by_user,error_type,error_code,
          error_message)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)
        ON CONFLICT(id) DO UPDATE SET
          session_id=excluded.session_id,
          turn_id=coalesce(excluded.turn_id,rust_tool_usage.turn_id),
          trace_id=coalesce(excluded.trace_id,rust_tool_usage.trace_id),
          tool_call_id=excluded.tool_call_id,
          tool_name=CASE WHEN excluded.tool_name='unknown' THEN rust_tool_usage.tool_name ELSE excluded.tool_name END,
          approval_status=coalesce(excluded.approval_status,rust_tool_usage.approval_status),
          status=CASE WHEN rust_tool_usage.status IN ('completed','error','cancelled') AND excluded.status='running'
            THEN rust_tool_usage.status ELSE excluded.status END,
          started_at=min(rust_tool_usage.started_at,excluded.started_at),
          completed_at=coalesce(excluded.completed_at,rust_tool_usage.completed_at),
          duration_ms=coalesce(excluded.duration_ms,rust_tool_usage.duration_ms),
          output_bytes=max(rust_tool_usage.output_bytes,excluded.output_bytes),
          cancelled_by_user=excluded.cancelled_by_user,
          error_type=coalesce(excluded.error_type,rust_tool_usage.error_type),
          error_code=coalesce(excluded.error_code,rust_tool_usage.error_code),
          error_message=coalesce(excluded.error_message,rust_tool_usage.error_message)"
            .replace("rust_", prefix),
    )?
    .execute(params![
        f.id(),
        f.session_id,
        f.turn_id,
        f.trace_id,
        f.tool_call_id,
        f.tool_name,
        f.approval_status,
        f.status,
        f.started_at as i64,
        f.completed_at.map(|at| at as i64),
        f.duration_ms.map(|ms| ms as i64),
        f.output_bytes as i64,
        f.cancelled_by_user,
        f.error.kind,
        f.error.code,
        f.error.message,
    ])?;
    Ok(())
}

/// The writer's pruning clock.
pub(super) struct Writer {
    pruned: Option<Instant>,
    /// The usage tables' prefix: `rust_` in the Rust store, none in the Node database.
    prefix: &'static str,
}

impl Default for Writer {
    fn default() -> Self {
        Self::new("rust_")
    }
}

impl Writer {
    pub(super) fn new(prefix: &'static str) -> Self {
        Self {
            pruned: None,
            prefix,
        }
    }

    /// One fact, then pruning when due. `now_ms` is the wall clock.
    pub(super) fn record(&mut self, conn: &Connection, fact: &Fact, now_ms: u64) -> Result<()> {
        match fact {
            Fact::Model(f) => model(conn, self.prefix, f)?,
            Fact::Turn(f) => turn(conn, self.prefix, f)?,
            Fact::Tool(f) => tool(conn, self.prefix, f)?,
        }
        if self.pruned.is_none_or(|at| at.elapsed() >= PRUNE_INTERVAL) {
            self.pruned = Some(Instant::now());
            prune(conn, self.prefix, now_ms.saturating_sub(RETENTION_MS))?;
        }
        Ok(())
    }
}

/// Node `pruneUsage`: rows started before `before` leave all three tables together.
pub(super) fn prune(conn: &Connection, prefix: &str, before: u64) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    for table in TABLES {
        tx.execute(
            &format!("DELETE FROM {prefix}{table} WHERE started_at < ?1"),
            [before as i64],
        )?;
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
#[path = "usage_tests.rs"]
mod tests;
