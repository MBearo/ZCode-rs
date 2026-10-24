//! 用量事实 → Node 用量表（docs/specs/rust-cli/usage.md §2，node-store.md §4.9）。
//!
//! 轮次与父消息取批次的回合锚点（`Commit::current_user`，同 assistant 记录）；trace、执行模式与会话类型从库里读
//! （`state`）；provider、模型与档位取请求时的快照。写过用量行的批次最后按 Node 的保留期删旧行（`facts::commit`）。
use rust_cli_kernel_api::message::Usage;
use rust_cli_kernel_api::metering::{CallStatus, ModelCall, ToolCall};
use rust_cli_kernel_api::model::ModelError;
use rust_cli_kernel_api::store::{Fact, StoreError};

use crate::append::finish_reason;
use crate::facts::Commit;
use crate::ids::created_at;
use crate::records::AGENT;
use crate::repo::usage::{self, ModelRow, ToolRow, TurnRow};
use crate::repo::{messages, sessions};
use crate::state;
use crate::{OrStorage, storage};

/// 这一批里有用量事实：提交前按保留期删旧行。
pub fn touches_usage(facts: &[Fact]) -> bool {
    facts.iter().any(|fact| {
        matches!(
            fact,
            Fact::ModelCalled { .. } | Fact::ToolCalled { .. } | Fact::TurnEnded { .. }
        )
    })
}

/// Node `pruneUsage`：`started_at` 早于提交时间 30 天的行。
pub fn prune(ctx: &Commit<'_>) -> Result<(), StoreError> {
    let now = i64::try_from(ctx.now).unwrap_or(i64::MAX);
    usage::prune(ctx.conn, now.saturating_sub(usage::RETENTION_MS)).storage()
}

/// 用量是观测（usage.md §2.5，工作线 F77）：`write` 在一个 savepoint 里执行；失败时只回退这个 savepoint、记 warn
/// （`event` 用 Node 的名字），返回 Ok，这一批其余的事实照常提交（Node 的用量写入失败也只记日志）。
///
/// 缺陷修复：以前用量行失败时整批回滚——主请求的回复不落库、回合以存储错误结束，日志里没有一行（库里的
/// `model_usage` 缺 `variant` 列时每个回合都这样）。SQLite 已经自己回滚了整个事务（磁盘满、IO 错误等）时 savepoint
/// 不在了，`rollback to` 失败，返回原来的错误，整批照旧失败。
pub fn observe(
    ctx: &Commit<'_>,
    event: &'static str,
    write: impl FnOnce() -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    ctx.conn
        .execute_batch("savepoint rust_cli_usage")
        .storage()?;
    let Err(error) = write() else {
        return ctx.conn.execute_batch("release rust_cli_usage").storage();
    };
    if ctx
        .conn
        .execute_batch("rollback to rust_cli_usage; release rust_cli_usage")
        .is_err()
    {
        return Err(error);
    }
    tracing::warn!(event, session = %ctx.session, reason = %error, "the usage was not stored");
    Ok(())
}

fn status(status: CallStatus) -> &'static str {
    match status {
        CallStatus::Completed => "completed",
        CallStatus::Error => "error",
        CallStatus::Cancelled => "cancelled",
    }
}

fn int(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// Node 归一化用量（AI SDK：输入含缓存读写，`totalTokens` = 输入 + 输出），与 assistant 的 `tokens` 同一来源。
struct Normalized {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
}

impl Normalized {
    fn of(usage: &Usage) -> Self {
        Self {
            input: usage
                .input_tokens
                .saturating_add(usage.cache_creation_input_tokens)
                .saturating_add(usage.cache_read_input_tokens),
            output: usage.output_tokens,
            cache_read: usage.cache_read_input_tokens,
            cache_write: usage.cache_creation_input_tokens,
        }
    }

    fn total(&self) -> u64 {
        self.input.saturating_add(self.output)
    }

    /// Node `inputSideTokensFromNormalizedUsage` + 输出（`recordModelUsage` 的 `computedTotalTokens`）。
    fn computed_total(&self) -> u64 {
        let side = if self.input > 0 {
            self.input
        } else {
            self.cache_write.saturating_add(self.cache_read)
        };
        side.saturating_add(self.output)
    }

    /// Node `encodeJson(normalizeUsage(...))`：Anthropic 没有 reasoning 与 server tool 用量，省略。
    fn json(&self) -> String {
        serde_json::json!({
            "inputTokens": self.input,
            "outputTokens": self.output,
            "totalTokens": self.total(),
            "cacheReadTokens": self.cache_read,
            "cacheWriteTokens": self.cache_write,
        })
        .to_string()
    }
}

/// 错误的种类（`ModelError` 的 serde tag）、5xx 的状态码与是否可重试（usage.md §2.2、D5）。
fn error_info(error: &ModelError) -> (String, Option<String>, bool) {
    let kind = serde_json::to_value(error)
        .ok()
        .and_then(|value| value["kind"].as_str().map(str::to_owned))
        .unwrap_or_default();
    let code = match error {
        ModelError::Server { status, .. } => Some(status.to_string()),
        _ => None,
    };
    let retryable = matches!(
        error,
        ModelError::RateLimited { .. }
            | ModelError::Overloaded { .. }
            | ModelError::Network { .. }
            | ModelError::Server { .. }
    );
    (kind, code, retryable)
}

/// 会话行与当前轮：trace、会话类型、当前的真实用户输入（id 与 `anchor.turnId`）。
struct Turn {
    trace: Option<String>,
    task_type: String,
    mode: String,
    user: Option<(String, Option<String>)>,
}

fn turn(ctx: &Commit<'_>) -> Result<Turn, StoreError> {
    let row = sessions::get(ctx.conn, ctx.session)
        .storage()?
        .ok_or_else(|| storage(format!("session {} disappeared", ctx.session)))?;
    let execution = state::execution(ctx.conn, &row)
        .storage()?
        .unwrap_or_default();
    Ok(Turn {
        trace: row.trace_id,
        task_type: row.task_type,
        mode: execution.mode,
        user: ctx.current_user()?,
    })
}

/// `ModelCalled` → `model_usage`。assistant 消息排在同一批的前面：库里有这条消息时写
/// `assistant_message_id`，没有提交消息（失败、丢弃的响应）时为 NULL（usage.md §2.2）。
pub fn model_called(ctx: &Commit<'_>, call: &ModelCall) -> Result<(), StoreError> {
    let turn = turn(ctx)?;
    let stored = messages::exists(ctx.conn, call.message.as_str()).storage()?;
    // Node `querySourceForTask` 与旁路请求的名字。修 bug（F71）：以前旁路请求照写本工程的名字（标题
    // `generate_session_title`、WebFetch 摘要 `web_fetch_apply`），与 Node 的 `session_title`、`web_fetch_processing`
    // 不同；现在与请求头共用 kernel-api 的 `QuerySource::node_name`。已有的行不改写。
    let source = call.source.node_name();
    let normalized = call.usage.as_ref().map(Normalized::of);
    let started = int(call.started_at_ms);
    let completed = int(call.completed_at_ms);
    let first = call.first_token_at_ms.map(int);
    let (error_type, error_code, error_retryable) = match &call.error {
        Some(error) => {
            let (kind, code, retryable) = error_info(error);
            (Some(kind), code, Some(retryable))
        }
        None => (None, None, None),
    };
    let message = call.message.as_str();
    let row = ModelRow {
        id: format!("usage_model_{source}_{message}_0"),
        logical_request_id: message.to_owned(),
        attempt_index: 0,
        session_id: ctx.session.to_owned(),
        turn_id: turn.user.as_ref().and_then(|(_, turn)| turn.clone()),
        trace_id: turn.trace,
        span_id: None,
        assistant_message_id: stored.then(|| message.to_owned()),
        parent_user_message_id: turn.user.map(|(id, _)| id),
        query_source: source,
        provider_id: call.selection.provider.clone(),
        model_id: call.selection.model.clone(),
        variant: call.selection.reasoning.clone().filter(|v| !v.is_empty()),
        agent: Some(AGENT.to_owned()),
        mode: Some(turn.mode),
        task_type: Some(turn.task_type),
        status: status(call.status),
        started_at: started,
        first_token_at: first,
        completed_at: Some(completed),
        duration_ms: Some(completed.saturating_sub(started)),
        time_to_first_token_ms: first.map(|first| first.saturating_sub(started)),
        // Node 只在有结果时写 `result.finishReason`。
        finish_reason: if call.status == CallStatus::Completed {
            call.stop_reason.map(finish_reason)
        } else {
            None
        },
        tool_call_count: i64::from(call.tool_calls),
        input_tokens: normalized.as_ref().map_or(0, |n| int(n.input)),
        output_tokens: normalized.as_ref().map_or(0, |n| int(n.output)),
        reasoning_tokens: 0,
        cache_creation_input_tokens: normalized.as_ref().map_or(0, |n| int(n.cache_write)),
        cache_read_input_tokens: normalized.as_ref().map_or(0, |n| int(n.cache_read)),
        provider_total_tokens: normalized.as_ref().map(|n| int(n.total())),
        computed_total_tokens: normalized.as_ref().map_or(0, |n| int(n.computed_total())),
        retry_count: i64::from(call.retries),
        // Node `errorInfo.retryable ?? retryCount > 0`。
        retryable: error_retryable.unwrap_or(call.retries > 0),
        cancelled_by_user: call.status == CallStatus::Cancelled,
        context_exceeded: matches!(call.error, Some(ModelError::PromptTooLong { .. })),
        error_type,
        error_code,
        error_message: call.error.as_ref().map(ToString::to_string),
        raw_usage_json: normalized.as_ref().map(Normalized::json),
        provider_metadata_json: None,
    };
    usage::record_model(ctx.conn, &row).storage()
}

/// `ToolCalled` → `tool_usage`（Node `recordToolUsageFromResult` 的最终形状，usage.md §2.3）。
pub fn tool_called(ctx: &Commit<'_>, call: &ToolCall) -> Result<(), StoreError> {
    let turn = turn(ctx)?;
    let started = int(call.started_at_ms);
    let completed = int(call.completed_at_ms).max(started);
    let duration = completed - started;
    let row = ToolRow {
        id: format!("usage_tool_{}_{}", ctx.session, call.tool_use),
        session_id: ctx.session.to_owned(),
        turn_id: turn.user.and_then(|(_, turn)| turn),
        trace_id: turn.trace,
        tool_call_id: call.tool_use.as_str().to_owned(),
        tool_name: call.name.clone(),
        approval_status: Some("none"),
        // Node 结算时只有 completed / error；用户中断是 error 加 `cancelledByUser`。
        status: if call.status == CallStatus::Completed {
            "completed"
        } else {
            "error"
        },
        started_at: started,
        first_output_at: Some(completed),
        completed_at: Some(completed),
        duration_ms: Some(duration),
        time_to_first_output_ms: Some(duration),
        output_bytes: int(call.output_bytes),
        cancelled_by_user: call.status == CallStatus::Cancelled,
    };
    usage::upsert_tool(ctx.conn, &row).storage()
}

/// `TurnEnded` → `turn_usage`：当前轮的输入消息与本轮用量行的聚合；没有真实用户输入时不写（usage.md §2.4）。
pub fn turn_ended(ctx: &Commit<'_>, ended: CallStatus) -> Result<(), StoreError> {
    let turn = turn(ctx)?;
    let Some((user, Some(turn_id))) = turn.user else {
        return Ok(());
    };
    let now = int(ctx.now);
    let input = int(created_at(&user, ctx.now));
    // 后台通知的回合与上一个人类输入同一轮（D7）：开始时间取表里已有的较早值，时长与它一致。
    let started = usage::turn_started(ctx.conn, ctx.session, &turn_id)
        .storage()?
        .map_or(input, |existing| existing.min(input));
    let totals = usage::turn_totals(ctx.conn, ctx.session, &turn_id).storage()?;
    let row = TurnRow {
        session_id: ctx.session.to_owned(),
        trace_id: turn.trace,
        user_message_id: Some(user),
        status: status(ended),
        started_at: started,
        first_model_start_at: totals.first_start,
        first_token_at: totals.first_token,
        completed_at: Some(now),
        duration_ms: Some(now.saturating_sub(started)),
        time_to_first_token_ms: totals
            .first_token
            .map(|first| first.saturating_sub(started)),
        model_request_count: totals.requests,
        model_retry_count: totals.retries,
        tool_call_count: totals.tools,
        tool_error_count: totals.tool_errors,
        input_tokens: totals.input,
        output_tokens: totals.output,
        reasoning_tokens: totals.reasoning,
        cache_creation_input_tokens: totals.cache_write,
        cache_read_input_tokens: totals.cache_read,
        computed_total_tokens: totals.total,
        cancelled_by_user: ended == CallStatus::Cancelled,
        context_exceeded: totals.context_exceeded,
        turn_id,
    };
    usage::upsert_turn(ctx.conn, &row).storage()
}

#[cfg(test)]
#[path = "usage_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "usage_observe_tests.rs"]
mod observe_tests;
