//! One conversation's token usage (Node `queryTaskUsage`).
use serde_json::{Map, Value, json};

/// One stored model request, in `started_at, id` order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TaskRow {
    pub query_source: String,
    pub status: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub computed_total_tokens: u64,
    pub provider_total_tokens: Option<u64>,
}

impl TaskRow {
    /// Node `inputSideTokensFromStoredUsage`: whether the cache fields are a
    /// breakdown of the input or come on top of it, judged by the total.
    fn input_side(&self) -> u64 {
        let input = self.input_tokens;
        let cache = self.cache_creation_tokens + self.cache_read_tokens;
        if input == 0 {
            return cache;
        }
        if cache == 0 {
            return input;
        }
        let total = self
            .provider_total_tokens
            .unwrap_or(self.computed_total_tokens) as i128;
        let output = self.output_tokens as i128;
        if total > 0 {
            let with_input = (total - (input as i128 + output)).abs();
            let with_cache = (total - (input as i128 + cache as i128 + output)).abs();
            if with_cache < with_input {
                return input + cache;
            }
        }
        input
    }
}

/// Node `taskUsageInputBaselineSource`.
fn baseline_source(source: &str) -> Option<&str> {
    matches!(source, "main_turn" | "subagent" | "workflow_child").then_some(source)
}

/// Node `queryTaskUsage` projected as the protocol result.
pub fn task_usage(session_id: &str, rows: &[TaskRow]) -> Value {
    let (mut total, mut input, mut output, mut reasoning) = (0u64, 0u64, 0u64, 0u64);
    let (mut cache_creation, mut cache_read, mut errors) = (0u64, 0u64, 0u64);
    let mut baselines = Map::new();
    for row in rows {
        let raw_total = row
            .provider_total_tokens
            .unwrap_or(row.computed_total_tokens);
        let input_side = row.input_side();
        let source = baseline_source(&row.query_source);
        let incremental = match source {
            Some(source) => {
                let baseline = baselines.get(source).and_then(Value::as_u64).unwrap_or(0);
                input_side.saturating_sub(baseline)
            }
            None => input_side,
        };
        if let Some(source) = source {
            // 压缩会让后续输入变小：累计不回扣，但基线降到压缩后的值（Node 同样处理）。
            baselines.insert(source.into(), input_side.into());
        }
        total += incremental + raw_total.saturating_sub(input_side);
        input += incremental;
        output += row.output_tokens;
        reasoning += row.reasoning_tokens;
        if source.is_none() {
            cache_creation += row.cache_creation_tokens;
            cache_read += row.cache_read_tokens;
        }
        if row.status == "error" {
            errors += 1;
        }
    }
    json!({
        "sessionId": session_id,
        "totalTokens": total,
        "inputTokens": input,
        "outputTokens": output,
        "reasoningTokens": reasoning,
        "cacheCreationTokens": cache_creation,
        "cacheReadTokens": cache_read,
        "modelRequestCount": rows.len(),
        "modelErrorCount": errors,
        "inputBaselineBySource": baselines,
    })
}
