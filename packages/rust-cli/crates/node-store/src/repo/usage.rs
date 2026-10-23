//! Node 用量表的原文 SQL（`apps/zcode-cli/packages/adapters/src/storage/session-store/repositories/usage.ts`：
//! `recordModelUsage`、`upsertTurnUsage`、`upsertToolUsage`、`pruneUsage`，docs/specs/rust-cli/usage.md §2）。
//!
//! 只有 SQL 与参数绑定；取值规则在 `crate::usage`。所有函数都在存储事务里调用。
use anyhow::Result;
use rusqlite::{Connection, params};

/// Node `USAGE_RETENTION_DAYS`。
pub const RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// `model_usage` 的一行（列按 Node 插入的顺序）。
#[derive(Debug, Clone, Default)]
pub struct ModelRow {
    pub id: String,
    pub logical_request_id: String,
    pub attempt_index: i64,
    pub session_id: String,
    pub turn_id: Option<String>,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub assistant_message_id: Option<String>,
    pub parent_user_message_id: Option<String>,
    pub query_source: String,
    pub provider_id: String,
    pub model_id: String,
    pub variant: Option<String>,
    pub agent: Option<String>,
    pub mode: Option<String>,
    pub task_type: Option<String>,
    pub status: &'static str,
    pub started_at: i64,
    pub first_token_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub duration_ms: Option<i64>,
    pub time_to_first_token_ms: Option<i64>,
    pub finish_reason: Option<&'static str>,
    pub tool_call_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    pub cache_creation_input_tokens: i64,
    pub cache_read_input_tokens: i64,
    pub provider_total_tokens: Option<i64>,
    pub computed_total_tokens: i64,
    pub retry_count: i64,
    pub retryable: bool,
    pub cancelled_by_user: bool,
    pub context_exceeded: bool,
    pub error_type: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub raw_usage_json: Option<String>,
    pub provider_metadata_json: Option<String>,
}

/// Node `recordModelUsage`（不含它之后的 `pruneUsage`，见 [`prune`]）。
pub fn record_model(conn: &Connection, row: &ModelRow) -> Result<()> {
    conn.prepare_cached(
        "
      insert into model_usage (
        id,
        logical_request_id,
        attempt_index,
        session_id,
        turn_id,
        trace_id,
        span_id,
        assistant_message_id,
        parent_user_message_id,
        query_source,
        provider_id,
        model_id,
        variant,
        agent,
        mode,
        task_type,
        status,
        started_at,
        first_token_at,
        completed_at,
        duration_ms,
        time_to_first_token_ms,
        finish_reason,
        tool_call_count,
        input_tokens,
        output_tokens,
        reasoning_tokens,
        cache_creation_input_tokens,
        cache_read_input_tokens,
        provider_total_tokens,
        computed_total_tokens,
        retry_count,
        retryable,
        cancelled_by_user,
        context_exceeded,
        error_type,
        error_code,
        error_message,
        raw_usage_json,
        provider_metadata_json
      )
      values (
        ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
        ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
        ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
        ?, ?, ?, ?, ?, ?, ?, ?, ?, ?
      )
      on conflict(id) do update set
        logical_request_id = excluded.logical_request_id,
        attempt_index = excluded.attempt_index,
        session_id = excluded.session_id,
        turn_id = excluded.turn_id,
        trace_id = excluded.trace_id,
        span_id = excluded.span_id,
        assistant_message_id = excluded.assistant_message_id,
        parent_user_message_id = excluded.parent_user_message_id,
        query_source = excluded.query_source,
        provider_id = excluded.provider_id,
        model_id = excluded.model_id,
        variant = excluded.variant,
        agent = excluded.agent,
        mode = excluded.mode,
        task_type = excluded.task_type,
        status = excluded.status,
        started_at = excluded.started_at,
        first_token_at = excluded.first_token_at,
        completed_at = excluded.completed_at,
        duration_ms = excluded.duration_ms,
        time_to_first_token_ms = excluded.time_to_first_token_ms,
        finish_reason = excluded.finish_reason,
        tool_call_count = excluded.tool_call_count,
        input_tokens = excluded.input_tokens,
        output_tokens = excluded.output_tokens,
        reasoning_tokens = excluded.reasoning_tokens,
        cache_creation_input_tokens = excluded.cache_creation_input_tokens,
        cache_read_input_tokens = excluded.cache_read_input_tokens,
        provider_total_tokens = excluded.provider_total_tokens,
        computed_total_tokens = excluded.computed_total_tokens,
        retry_count = excluded.retry_count,
        retryable = excluded.retryable,
        cancelled_by_user = excluded.cancelled_by_user,
        context_exceeded = excluded.context_exceeded,
        error_type = excluded.error_type,
        error_code = excluded.error_code,
        error_message = excluded.error_message,
        raw_usage_json = excluded.raw_usage_json,
        provider_metadata_json = excluded.provider_metadata_json
      ",
    )?
    .execute(params![
        row.id,
        row.logical_request_id,
        row.attempt_index,
        row.session_id,
        row.turn_id,
        row.trace_id,
        row.span_id,
        row.assistant_message_id,
        row.parent_user_message_id,
        row.query_source,
        row.provider_id,
        row.model_id,
        row.variant,
        row.agent,
        row.mode,
        row.task_type,
        row.status,
        row.started_at,
        row.first_token_at,
        row.completed_at,
        row.duration_ms,
        row.time_to_first_token_ms,
        row.finish_reason,
        row.tool_call_count,
        row.input_tokens,
        row.output_tokens,
        row.reasoning_tokens,
        row.cache_creation_input_tokens,
        row.cache_read_input_tokens,
        row.provider_total_tokens,
        row.computed_total_tokens,
        row.retry_count,
        i64::from(row.retryable),
        i64::from(row.cancelled_by_user),
        i64::from(row.context_exceeded),
        row.error_type,
        row.error_code,
        row.error_message,
        row.raw_usage_json,
        row.provider_metadata_json,
    ])?;
    Ok(())
}

/// `tool_usage` 的一行（Node `toolUsageValues` 的顺序）。
#[derive(Debug, Clone, Default)]
pub struct ToolRow {
    pub id: String,
    pub session_id: String,
    pub turn_id: Option<String>,
    pub trace_id: Option<String>,
    pub tool_call_id: String,
    pub tool_name: String,
    pub approval_status: Option<&'static str>,
    pub status: &'static str,
    pub started_at: i64,
    pub first_output_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub duration_ms: Option<i64>,
    pub time_to_first_output_ms: Option<i64>,
    pub output_bytes: i64,
    pub cancelled_by_user: bool,
}

/// Node `upsertToolUsage`。本工程没有的列（副作用范围、只读、破坏性、退出码、stdout / stderr 字节、错误）
/// 绑定 Node 在缺省时的值（usage.md §2.3、D6）。
pub fn upsert_tool(conn: &Connection, row: &ToolRow) -> Result<()> {
    conn.prepare_cached(
        "
      insert into tool_usage (
        id,
        session_id,
        turn_id,
        trace_id,
        tool_call_id,
        tool_name,
        side_effect_scope,
        read_only,
        destructive,
        approval_status,
        status,
        started_at,
        first_output_at,
        completed_at,
        duration_ms,
        time_to_first_output_ms,
        exit_code,
        output_bytes,
        stdout_bytes,
        stderr_bytes,
        truncated,
        retry_count,
        retryable,
        cancelled_by_user,
        error_type,
        error_code,
        error_message
      )
      values (
        ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
        ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
        ?, ?, ?, ?, ?, ?, ?
      )
      on conflict(id) do update set
        session_id = excluded.session_id,
        turn_id = coalesce(excluded.turn_id, tool_usage.turn_id),
        trace_id = coalesce(excluded.trace_id, tool_usage.trace_id),
        tool_call_id = excluded.tool_call_id,
        tool_name = case
          when excluded.tool_name = 'unknown' then tool_usage.tool_name
          else excluded.tool_name
        end,
        side_effect_scope = coalesce(excluded.side_effect_scope, tool_usage.side_effect_scope),
        read_only = coalesce(excluded.read_only, tool_usage.read_only),
        destructive = coalesce(excluded.destructive, tool_usage.destructive),
        approval_status = coalesce(excluded.approval_status, tool_usage.approval_status),
        status = case
          when tool_usage.status in ('completed', 'error', 'cancelled') and excluded.status = 'running'
            then tool_usage.status
          else excluded.status
        end,
        started_at = min(tool_usage.started_at, excluded.started_at),
        first_output_at = coalesce(tool_usage.first_output_at, excluded.first_output_at),
        completed_at = coalesce(excluded.completed_at, tool_usage.completed_at),
        duration_ms = coalesce(excluded.duration_ms, tool_usage.duration_ms),
        time_to_first_output_ms = coalesce(excluded.time_to_first_output_ms, tool_usage.time_to_first_output_ms),
        exit_code = coalesce(excluded.exit_code, tool_usage.exit_code),
        output_bytes = max(tool_usage.output_bytes, excluded.output_bytes),
        stdout_bytes = max(tool_usage.stdout_bytes, excluded.stdout_bytes),
        stderr_bytes = max(tool_usage.stderr_bytes, excluded.stderr_bytes),
        truncated = max(tool_usage.truncated, excluded.truncated),
        retry_count = excluded.retry_count,
        retryable = excluded.retryable,
        cancelled_by_user = excluded.cancelled_by_user,
        error_type = coalesce(excluded.error_type, tool_usage.error_type),
        error_code = coalesce(excluded.error_code, tool_usage.error_code),
        error_message = coalesce(excluded.error_message, tool_usage.error_message)
      ",
    )?
    .execute(params![
        row.id,
        row.session_id,
        row.turn_id,
        row.trace_id,
        row.tool_call_id,
        row.tool_name,
        None::<String>,
        None::<i64>,
        None::<i64>,
        row.approval_status,
        row.status,
        row.started_at,
        row.first_output_at,
        row.completed_at,
        row.duration_ms,
        row.time_to_first_output_ms,
        None::<i64>,
        row.output_bytes,
        0_i64,
        0_i64,
        0_i64,
        0_i64,
        0_i64,
        i64::from(row.cancelled_by_user),
        None::<String>,
        None::<String>,
        None::<String>,
    ])?;
    Ok(())
}

/// `turn_usage` 的一行（Node 插入的顺序）。
#[derive(Debug, Clone, Default)]
pub struct TurnRow {
    pub session_id: String,
    pub turn_id: String,
    pub trace_id: Option<String>,
    pub user_message_id: Option<String>,
    pub status: &'static str,
    pub started_at: i64,
    pub first_model_start_at: Option<i64>,
    pub first_token_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub duration_ms: Option<i64>,
    pub time_to_first_token_ms: Option<i64>,
    pub model_request_count: i64,
    pub model_retry_count: i64,
    pub tool_call_count: i64,
    pub tool_error_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    pub cache_creation_input_tokens: i64,
    pub cache_read_input_tokens: i64,
    pub computed_total_tokens: i64,
    pub cancelled_by_user: bool,
    pub context_exceeded: bool,
}

/// Node `upsertTurnUsage`（`retryable`、错误列绑定缺省值，usage.md §2.4）。
pub fn upsert_turn(conn: &Connection, row: &TurnRow) -> Result<()> {
    conn.prepare_cached(
        "
      insert into turn_usage (
        session_id,
        turn_id,
        trace_id,
        user_message_id,
        status,
        started_at,
        first_model_start_at,
        first_token_at,
        completed_at,
        duration_ms,
        time_to_first_token_ms,
        model_request_count,
        model_retry_count,
        tool_call_count,
        tool_error_count,
        input_tokens,
        output_tokens,
        reasoning_tokens,
        cache_creation_input_tokens,
        cache_read_input_tokens,
        computed_total_tokens,
        retryable,
        cancelled_by_user,
        context_exceeded,
        error_type,
        error_code
      )
      values (
        ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
        ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
        ?, ?, ?, ?, ?, ?
      )
      on conflict(session_id, turn_id) do update set
        trace_id = coalesce(excluded.trace_id, turn_usage.trace_id),
        user_message_id = coalesce(excluded.user_message_id, turn_usage.user_message_id),
        status = excluded.status,
        started_at = min(turn_usage.started_at, excluded.started_at),
        first_model_start_at = coalesce(turn_usage.first_model_start_at, excluded.first_model_start_at),
        first_token_at = coalesce(turn_usage.first_token_at, excluded.first_token_at),
        completed_at = coalesce(excluded.completed_at, turn_usage.completed_at),
        duration_ms = coalesce(excluded.duration_ms, turn_usage.duration_ms),
        time_to_first_token_ms = coalesce(excluded.time_to_first_token_ms, turn_usage.time_to_first_token_ms),
        model_request_count = excluded.model_request_count,
        model_retry_count = excluded.model_retry_count,
        tool_call_count = excluded.tool_call_count,
        tool_error_count = excluded.tool_error_count,
        input_tokens = excluded.input_tokens,
        output_tokens = excluded.output_tokens,
        reasoning_tokens = excluded.reasoning_tokens,
        cache_creation_input_tokens = excluded.cache_creation_input_tokens,
        cache_read_input_tokens = excluded.cache_read_input_tokens,
        computed_total_tokens = excluded.computed_total_tokens,
        retryable = excluded.retryable,
        cancelled_by_user = excluded.cancelled_by_user,
        context_exceeded = excluded.context_exceeded,
        error_type = coalesce(excluded.error_type, turn_usage.error_type),
        error_code = coalesce(excluded.error_code, turn_usage.error_code)
      ",
    )?
    .execute(params![
        row.session_id,
        row.turn_id,
        row.trace_id,
        row.user_message_id,
        row.status,
        row.started_at,
        row.first_model_start_at,
        row.first_token_at,
        row.completed_at,
        row.duration_ms,
        row.time_to_first_token_ms,
        row.model_request_count,
        row.model_retry_count,
        row.tool_call_count,
        row.tool_error_count,
        row.input_tokens,
        row.output_tokens,
        row.reasoning_tokens,
        row.cache_creation_input_tokens,
        row.cache_read_input_tokens,
        row.computed_total_tokens,
        0_i64,
        i64::from(row.cancelled_by_user),
        i64::from(row.context_exceeded),
        None::<String>,
        None::<String>,
    ])?;
    Ok(())
}

/// Node `pruneUsage` 的三条 delete（Node 自己再开一个 `begin immediate`；这里已在提交的事务里）。
pub fn prune(conn: &Connection, before: i64) -> Result<()> {
    conn.prepare_cached("delete from model_usage where started_at < ?")?
        .execute([before])?;
    conn.prepare_cached("delete from turn_usage where started_at < ?")?
        .execute([before])?;
    conn.prepare_cached("delete from tool_usage where started_at < ?")?
        .execute([before])?;
    Ok(())
}

/// 一轮已有的用量行的聚合（`turn_usage` 的计数与 token，usage.md §2.4）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnTotals {
    pub requests: i64,
    pub retries: i64,
    pub input: i64,
    pub output: i64,
    pub reasoning: i64,
    pub cache_write: i64,
    pub cache_read: i64,
    pub total: i64,
    pub first_start: Option<i64>,
    pub first_token: Option<i64>,
    pub context_exceeded: bool,
    pub tools: i64,
    pub tool_errors: i64,
}

/// 按 `(session_id, turn_id)` 聚合（索引 `model_usage_session_turn_idx`、`tool_usage_session_turn_idx`）。
pub fn turn_totals(conn: &Connection, session: &str, turn: &str) -> Result<TurnTotals> {
    let mut totals = conn
        .prepare_cached(
            "select count(*), coalesce(sum(retry_count), 0), coalesce(sum(input_tokens), 0),
               coalesce(sum(output_tokens), 0), coalesce(sum(reasoning_tokens), 0),
               coalesce(sum(cache_creation_input_tokens), 0), coalesce(sum(cache_read_input_tokens), 0),
               coalesce(sum(computed_total_tokens), 0), min(started_at), min(first_token_at),
               coalesce(max(context_exceeded), 0)
             from model_usage where session_id = ? and turn_id = ?",
        )?
        .query_row([session, turn], |r| {
            Ok(TurnTotals {
                requests: r.get(0)?,
                retries: r.get(1)?,
                input: r.get(2)?,
                output: r.get(3)?,
                reasoning: r.get(4)?,
                cache_write: r.get(5)?,
                cache_read: r.get(6)?,
                total: r.get(7)?,
                first_start: r.get(8)?,
                first_token: r.get(9)?,
                context_exceeded: r.get::<_, i64>(10)? != 0,
                ..TurnTotals::default()
            })
        })?;
    (totals.tools, totals.tool_errors) = conn
        .prepare_cached(
            "select count(*), coalesce(sum(case when status = 'error' then 1 else 0 end), 0)
             from tool_usage where session_id = ? and turn_id = ?",
        )?
        .query_row([session, turn], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(totals)
}

/// 这一轮已有的 `turn_usage.started_at`（主键查询）。
pub fn turn_started(conn: &Connection, session: &str, turn: &str) -> Result<Option<i64>> {
    use rusqlite::OptionalExtension;
    Ok(conn
        .prepare_cached("select started_at from turn_usage where session_id = ? and turn_id = ?")?
        .query_row([session, turn], |r| r.get(0))
        .optional()?)
}
