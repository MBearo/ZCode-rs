//! Usage facts and their aggregation (Node usage store, `usage-observability.ts`
//! and `usage-stats-builder.ts`), without IO. Spec rust-m9-usage-logs §2.
mod context;
mod metadata;
mod snapshot;
mod task;
mod tracker;
mod tz;

pub use context::{CacheHits, CacheUse, SectionChars, breakdown, context_tokens, js_len};
pub use metadata::{ToolMeta, model_usage, tool_meta};
pub use snapshot::{AppRows, DayModelRow, DayRow, ModelRow, SnapshotOptions, ToolRow, snapshot};
pub use task::{TaskRow, task_usage};
pub use tracker::{Attribution, Outcome, RunUsage, StepIds, ToolEnd};
pub use tz::offset_ms;

use serde_json::Value;

/// Query sources whose model requests are recorded (Node records these and
/// not the tool-internal requests such as `web_fetch_processing`).
pub const RECORDED_SOURCES: [&str; 4] = [
    "main_turn",
    "subagent",
    "compact",
    "target_completion_verification",
];
/// The private `model_request_completed` member with the provider's finish
/// reason (Node `providerMetadata.rawFinishReason`); removed before the status
/// leaves the runtime.
pub const RAW_FINISH_REASON: &str = "_zcode_raw_finish_reason";
/// Rows older than this are pruned (Node `USAGE_RETENTION_DAYS`).
pub const RETENTION_MS: u64 = 30 * 86_400_000;
pub const DAY_MS: i64 = 86_400_000;

/// Node `integer()`: finite numbers truncated and clamped at 0, others 0.
pub fn integer(value: &Value) -> u64 {
    match value.as_f64() {
        Some(n) if n.is_finite() && n > 0.0 => n.trunc() as u64,
        _ => 0,
    }
}

/// A JS number: integral values without a fraction (`100`, not `100.0`).
pub fn js_number(value: f64) -> Value {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 9_007_199_254_740_992.0 {
        (value as i64).into()
    } else {
        value.into()
    }
}

/// The token fields of one Node `ModelUsage`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub reasoning: u64,
    pub cache_write: u64,
    pub cache_read: u64,
}

impl Tokens {
    pub fn from_usage(usage: &Value) -> Self {
        Self {
            input: integer(&usage["inputTokens"]),
            output: integer(&usage["outputTokens"]),
            reasoning: integer(&usage["reasoningTokens"]),
            cache_write: integer(&usage["cacheWriteTokens"]),
            cache_read: integer(&usage["cacheReadTokens"]),
        }
    }

    /// Node `inputSideTokensFromNormalizedUsage`.
    pub fn input_side(&self) -> u64 {
        if self.input > 0 {
            self.input
        } else {
            self.cache_write + self.cache_read
        }
    }

    /// Node `computedTotalTokens` of a model request.
    pub fn computed_total(&self) -> u64 {
        self.input_side() + self.output
    }

    fn add(&mut self, other: &Self) {
        self.input += other.input;
        self.output += other.output;
        self.reasoning += other.reasoning;
        self.cache_write += other.cache_write;
        self.cache_read += other.cache_read;
    }
}

/// Node `getModelUsageTotalTokens` (turn totals).
pub fn usage_total(usage: &Value) -> u64 {
    if usage["totalTokens"].is_number() {
        return integer(&usage["totalTokens"]);
    }
    let input = match usage.get("inputTokens").filter(|v| !v.is_null()) {
        Some(input) => integer(input),
        None => integer(&usage["cacheReadTokens"]) + integer(&usage["cacheWriteTokens"]),
    };
    input + integer(&usage["outputTokens"])
}

/// A failure's Node `errorInfoFor` fields.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ErrorInfo {
    pub kind: Option<String>,
    pub code: Option<String>,
    pub message: Option<String>,
}

/// One Node `ModelUsageRecord`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelFact {
    pub id: String,
    pub logical_request_id: String,
    /// Compaction's prompt-too-long retry index (Node `attemptIndex`).
    pub attempt_index: u64,
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
    pub agent: String,
    pub mode: String,
    pub task_type: String,
    pub status: &'static str,
    pub started_at: u64,
    pub first_token_at: Option<u64>,
    pub completed_at: u64,
    pub finish_reason: Option<String>,
    pub tool_call_count: u64,
    pub tokens: Tokens,
    pub provider_total_tokens: Option<u64>,
    pub retry_count: u64,
    pub retryable: bool,
    pub context_exceeded: bool,
    pub error: ErrorInfo,
    pub raw_usage: Option<Value>,
    pub provider_metadata: Option<Value>,
}

/// One Node `TurnUsageRecord`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TurnFact {
    pub session_id: String,
    pub turn_id: String,
    pub trace_id: Option<String>,
    pub user_message_id: Option<String>,
    pub status: &'static str,
    pub started_at: u64,
    pub first_model_start_at: Option<u64>,
    pub first_token_at: Option<u64>,
    pub completed_at: u64,
    pub model_request_count: u64,
    pub model_retry_count: u64,
    pub tool_call_count: u64,
    pub tool_error_count: u64,
    pub tokens: Tokens,
    pub computed_total_tokens: u64,
    pub retryable: bool,
    pub cancelled_by_user: bool,
    pub context_exceeded: bool,
    pub error: ErrorInfo,
}

/// One Node `ToolUsageRecord` state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolFact {
    pub session_id: String,
    pub turn_id: Option<String>,
    pub trace_id: Option<String>,
    pub tool_call_id: String,
    pub tool_name: String,
    pub meta: Option<ToolMeta>,
    pub approval_status: &'static str,
    pub status: &'static str,
    pub started_at: u64,
    pub first_output_at: Option<u64>,
    pub completed_at: Option<u64>,
    pub duration_ms: Option<u64>,
    pub time_to_first_output_ms: Option<u64>,
    pub exit_code: Option<i64>,
    pub output_bytes: u64,
    pub truncated: bool,
    pub cancelled_by_user: bool,
    pub error: ErrorInfo,
}

impl ToolFact {
    /// Node `toolUsageId`.
    pub fn id(&self) -> String {
        format!("usage_tool_{}_{}", self.session_id, self.tool_call_id)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Fact {
    Model(Box<ModelFact>),
    Turn(Box<TurnFact>),
    Tool(Box<ToolFact>),
}

#[cfg(test)]
mod stats_tests;
#[cfg(test)]
mod tests;
