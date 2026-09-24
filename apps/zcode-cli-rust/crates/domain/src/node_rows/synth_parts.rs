//! Assistant parts → session events (Node `transcript-hydration.ts`
//! `synthesizeAssistantParts` and its part synthesizers).
use super::events::{iso, num};
use super::facts::{created_ms, finite, js_string, truthy};
use super::schemas;
use super::synth::Synth;
use super::synth_goals;
use super::synth_subagents::{subagent_info, subagent_lifecycle, subtask_part};
use crate::node_history::Record;
use serde_json::{Map, Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnResult {
    Success,
    Cancelled,
    Error,
}

impl TurnResult {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Cancelled => "cancelled",
            Self::Error => "error_during_execution",
        }
    }
    /// Node `normalizeTurnResult`: cancellation wins, then errors.
    pub fn and(self, next: Self) -> Self {
        match (self, next) {
            (Self::Cancelled, _) | (_, Self::Cancelled) => Self::Cancelled,
            (Self::Error, _) | (_, Self::Error) => Self::Error,
            _ => Self::Success,
        }
    }
}

const CANCELLATION_CODES: [&str; 4] = [
    "turn_cancelled",
    "model_request_cancelled",
    "MODEL_REQUEST_CANCELLED",
    "ABORT_ERR",
];

/// Node `assistantErrorData`.
pub fn error_data(error: &Value) -> Option<&Map<String, Value>> {
    error["data"].as_object()
}

/// Node `isPersistedAssistantCancellation`.
pub fn is_cancellation(error: &Value) -> bool {
    let data = error_data(error);
    let field = |key: &str| data.and_then(|d| d.get(key));
    let code = field("code").and_then(Value::as_str);
    if field("turnResult").is_some_and(|v| v == "cancelled")
        || field("resultType").is_some_and(|v| v == "cancelled")
        || code.is_some_and(|c| CANCELLATION_CODES.contains(&c))
        || error["name"] == "AbortError"
    {
        return true;
    }
    let message = field("message").and_then(Value::as_str);
    code.is_none()
        && ((error["name"] == "Error" && message == Some("ZCode Protocol session stopped"))
            || (error["name"] == "AiSdkModelAdapterError"
                && message == Some("Model request was cancelled.")))
}

/// Node `isPersistedStreamRecoveryDiscard`.
pub fn is_stream_recovery_discard(error: &Value) -> bool {
    error["name"] == "StreamRecoveryDiscarded"
}

/// Node `shouldHideInvalidToolCallFromProduct`.
pub fn hide_invalid_tool(name: &Value, metadata: Option<&Value>) -> bool {
    let Some(name) = name.as_str() else {
        return false;
    };
    if name.trim().is_empty() {
        return true;
    }
    name == "empty_tool_name"
        && metadata
            .and_then(|m| m["providerToolName"].as_str())
            .is_some_and(|provider| provider.trim().is_empty())
}

/// `Math.max(0, end - start)` over JS numbers (a missing side is NaN).
fn duration(time: &Value) -> Value {
    let span =
        finite(&time["end"]).unwrap_or(f64::NAN) - finite(&time["start"]).unwrap_or(f64::NAN);
    num(if span.is_nan() { span } else { span.max(0.0) })
}

fn schedule(id: &Value) -> Value {
    json!({"executionOrder": [id], "parallelGroups": [[id]]})
}

/// Inserts `key` only when the JS member is not undefined.
pub(super) fn put(map: &mut Value, key: &str, value: Option<&Value>) {
    if let Some(value) = value {
        map[key] = value.clone();
    }
}

fn text_part(s: &mut Synth, part: &Value, message: &str, created: Option<f64>, turn: &str) {
    let body = part["text"].as_str().unwrap_or("");
    if part["ignored"] == true || body.is_empty() {
        return;
    }
    let id = &part["id"];
    let start = json!({"kind": "text_start", "delta": "", "done": false, "assistantMessageId": message, "partId": id});
    s.push("model_streaming", start, Some(turn), created);
    let delta = json!({"kind": "text_delta", "delta": body, "done": false, "assistantMessageId": message, "partId": id});
    s.push("model_streaming", delta, Some(turn), None);
    let end = json!({"kind": "text_end", "delta": "", "done": false, "partId": id});
    s.push("model_streaming", end, Some(turn), None);
}

fn reasoning_part(s: &mut Synth, part: &Value, message: &str, turn: &str) {
    let body = part["text"].as_str().unwrap_or("");
    if body.is_empty() {
        return;
    }
    let id = &part["id"];
    let start = json!({"kind": "reasoning_start", "delta": "", "done": false, "assistantMessageId": message, "partId": id});
    s.push("model_streaming", start, Some(turn), None);
    let delta = json!({"kind": "reasoning_delta", "delta": body, "done": false, "partId": id});
    s.push("model_streaming", delta, Some(turn), None);
    let end = json!({"kind": "reasoning_end", "delta": "", "done": false, "partId": id});
    s.push("model_streaming", end, Some(turn), None);
}

/// Node `synthesizeToolPart`: `(result, toolCallCount)`.
fn tool_part(s: &mut Synth, part: &Value, message: &str, turn: &str) -> (TurnResult, u64) {
    if hide_invalid_tool(&part["tool"], part.get("metadata")) {
        return (TurnResult::Success, 0);
    }
    let state = &part["state"];
    let id = part.get("callID").cloned().unwrap_or(Value::Null);
    let metadata = match state.get("metadata") {
        Some(metadata) => metadata,
        None => &part["metadata"],
    };
    let display =
        schemas::completed_tool_metadata(metadata).and_then(|m| m.get("display").cloned());
    let mut scheduled = json!({"toolCallId": id, "assistantMessageId": message});
    put(&mut scheduled, "toolName", part.get("tool"));
    put(&mut scheduled, "input", state.get("input"));
    put(&mut scheduled, "display", display.as_ref());
    scheduled["schedule"] = schedule(&id);
    s.push("tool_call_scheduled", scheduled, Some(turn), None);

    let status = state["status"].as_str().unwrap_or("");
    let started = matches!(status, "running" | "completed" | "error");
    if started {
        let mut event = json!({"toolCallId": id});
        put(&mut event, "toolName", part.get("tool"));
        put(&mut event, "display", display.as_ref());
        event["startedAt"] = iso(finite(&state["time"]["start"]).unwrap_or(0.0));
        s.push("tool_call_started", event, Some(turn), None);
        if let Some(info) = subagent_info(part) {
            let lifecycle = match status {
                "completed" => "completed",
                "error" => "failed",
                _ => "cancelled",
            };
            subagent_lifecycle(s, info, lifecycle, turn);
        }
    }
    let result = match status {
        "completed" => {
            let mut result = json!({"success": true});
            put(&mut result, "content", state.get("output"));
            put(&mut result, "display", display.as_ref());
            result
        }
        "error" => json!({"success": false, "content": state["error"], "error": {
            "type": "fault.runtime.toolFailed", "message": state["error"]}}),
        // 重启后无法证明 pending/running 工具仍在运行：本轮按取消收口。
        _ => return (TurnResult::Cancelled, 1),
    };
    let payload = json!({"toolCallId": id, "duration": duration(&state["time"]), "result": result});
    s.push("tool_call_result", payload, Some(turn), None);
    (TurnResult::Success, 1)
}

/// Node `synthesizeAssistantParts`: `(result, toolCallCount)`.
pub fn assistant_parts(s: &mut Synth, record: &Record, turn: &str) -> (TurnResult, u64) {
    let info = &record.info;
    let mut result = match info.get("error").filter(|e| !e.is_null()) {
        Some(error) if is_cancellation(error) => TurnResult::Cancelled,
        Some(error) if is_stream_recovery_discard(error) => TurnResult::Success,
        Some(_) => TurnResult::Error,
        // 进程退出可能只落了部分 assistant 且没有 error：按中断收口，而不是伪造成功。
        None if info["role"] == "assistant" && info["time"].get("completed").is_none() => {
            TurnResult::Cancelled
        }
        None => TurnResult::Success,
    };
    let id = js_string(&info["id"]);
    let mut tool_calls = 0;
    for part in &record.parts {
        match part["type"].as_str() {
            Some("text") => text_part(s, part, &id, created_ms(record), turn),
            Some("reasoning") => reasoning_part(s, part, &id, turn),
            Some("tool") => {
                let (tool_result, count) = tool_part(s, part, &id, turn);
                tool_calls += count;
                result = result.and(tool_result);
            }
            Some("timeline") => {
                let goal = synth_goals::goal_part(s, part, turn);
                if !goal {
                    s.compact_part(part, turn);
                }
            }
            Some("compaction") => {
                s.compact_part(part, turn);
            }
            Some("subtask") => subtask_part(s, part, turn),
            _ => {}
        }
    }
    let feedback = &info["metadata"]["assistantFeedback"];
    if feedback == "like" || feedback == "dislike" {
        let payload = json!({"entityId": id, "feedback": feedback});
        s.push("assistant_feedback_updated", payload, Some(turn), None);
    }
    (result, tool_calls)
}

/// Node `assistantMessageHasSynthesizableContent`.
pub fn has_synthesizable_content(record: &Record) -> bool {
    record.parts.iter().any(|part| match part["type"].as_str() {
        Some("text") => part["ignored"] != true && truthy(&part["text"]),
        Some("reasoning") => truthy(&part["text"]),
        Some("tool") => !hide_invalid_tool(&part["tool"], part.get("metadata")),
        Some("subtask" | "compaction") => true,
        Some("timeline") => matches!(
            part["timelineType"].as_str(),
            Some("context_compaction" | "goal_verification")
        ),
        _ => false,
    })
}
