//! `hookInvocation` rows (Node product projection `onHookRunLifecycle`,
//! `hookBlockErrorDelta`, `onSessionResumed`).
use super::display::display_name;
use super::runner::Kind;
use serde_json::{Map, Value, json};

const PROMPT_BLOCK: &str = "hooks_prompt_block";

fn lane(event: &str) -> &'static str {
    match event {
        "PreToolUse" | "PermissionRequest" => "toolBefore",
        "PostToolUse" | "PostToolUseFailure" => "toolAfter",
        _ => "assistantWork",
    }
}

fn number(value: &Value) -> Option<u64> {
    value
        .as_f64()
        .filter(|n| n.is_finite())
        .map(|n| n.max(0.0) as u64)
}

/// The row content after one lifecycle event, merged over `existing` (the
/// invocation's row, if any). `None`: not projected (malformed or internal).
pub fn invocation(
    existing: Option<&Value>,
    kind: Kind,
    payload: &Value,
    now: u64,
) -> Option<Value> {
    let invocation_id = payload["hookInvocationId"]
        .as_str()
        .filter(|s| !s.is_empty())?;
    let run_id = payload["hookRunId"].as_str().filter(|s| !s.is_empty())?;
    let count = payload["hookCount"].as_u64().filter(|n| *n > 0)?;
    let index = payload["hookIndex"].as_u64()?;
    let previous_all: Vec<Value> = existing
        .and_then(|row| row["executions"].as_array().cloned())
        .unwrap_or_default();
    let previous = previous_all.iter().find(|e| e["hookRunId"] == run_id);
    let descriptor = payload.get("descriptor");
    if previous.is_none()
        && descriptor.is_none_or(|d| d["clientVisible"] != true || d["sourceKind"] == "internal")
    {
        return None;
    }
    let state = match kind {
        Kind::Failed => "failed",
        Kind::Completed | Kind::Blocked => "completed",
        Kind::Started => "running",
    };
    let started_at = number(&payload["startedAt"])
        .or_else(|| previous.and_then(|p| p["startedAt"].as_u64()))
        .unwrap_or(now);
    let ended_at = (state != "running").then_some(now);
    let duration = number(&payload["durationMs"])
        .or_else(|| ended_at.map(|end| end.saturating_sub(started_at)));
    let outcome = payload["outcome"].as_str().or(match kind {
        Kind::Completed => Some("success"),
        Kind::Blocked => Some("blocked"),
        Kind::Failed => Some("failed"),
        Kind::Started => None,
    });
    let did_execute = previous.is_some_and(|p| p["didExecute"] == true) || kind == Kind::Started;
    let source_kind = previous
        .and_then(|p| p["sourceKind"].as_str())
        .or_else(|| descriptor.and_then(|d| d["sourceKind"].as_str()))
        .filter(|kind| *kind != "internal")?;
    let block_reason = payload["blockReason"]
        .as_str()
        .or_else(|| previous.and_then(|p| p["blockReason"].as_str()));
    let mut execution = Map::new();
    execution.insert("hookRunId".into(), run_id.into());
    execution.insert("hookIndex".into(), index.into());
    execution.insert("didExecute".into(), did_execute.into());
    execution.insert("state".into(), state.into());
    if let Some(outcome) = outcome {
        execution.insert("outcome".into(), outcome.into());
    }
    if let Some(reason) = block_reason.filter(|r| !r.is_empty()) {
        execution.insert("blockReason".into(), reason.into());
    }
    execution.insert("startedAt".into(), started_at.into());
    if let Some(end) = ended_at {
        execution.insert("endedAt".into(), end.into());
    }
    if let Some(duration) = duration {
        execution.insert("durationMs".into(), duration.into());
    }
    let name = previous
        .and_then(|p| p["displayName"].as_str().map(str::to_owned))
        .unwrap_or_else(|| match descriptor {
            Some(d) => display_name(d, index),
            None => format!("Hook #{}", index + 1),
        });
    execution.insert("displayName".into(), name.into());
    execution.insert("sourceKind".into(), source_kind.into());
    let plugin = previous
        .and_then(|p| p["pluginName"].as_str())
        .or_else(|| descriptor.and_then(|d| d["pluginName"].as_str()))
        .filter(|s| !s.is_empty());
    if let Some(plugin) = plugin {
        execution.insert("pluginName".into(), plugin.into());
    }
    let tool = payload["toolName"]
        .as_str()
        .or_else(|| previous.and_then(|p| p["toolName"].as_str()))
        .filter(|s| !s.is_empty());
    if let Some(tool) = tool {
        execution.insert("toolName".into(), tool.into());
    }
    let mut executions = previous_all.clone();
    match executions.iter_mut().find(|e| e["hookRunId"] == run_id) {
        Some(slot) => *slot = Value::Object(execution),
        None => executions.push(Value::Object(execution)),
    }
    executions.sort_by_key(|e| e["hookIndex"].as_u64().unwrap_or(0));
    let row_state = if (executions.len() as u64) < count
        || executions.iter().any(|e| e["state"] == "running")
    {
        "running"
    } else if executions.iter().any(|e| e["state"] == "failed") {
        "failed"
    } else {
        "completed"
    };
    let starts = executions.iter().filter_map(|e| e["startedAt"].as_u64());
    let invocation_start = starts.min().unwrap_or(started_at);
    let mut content = json!({
        "kind": "hookInvocation",
        "hookInvocationId": invocation_id,
        "hookEventName": payload["hookEventName"],
        "hookCount": count,
        "state": row_state,
        "startedAt": invocation_start,
    });
    if row_state != "running" {
        let end = executions
            .iter()
            .filter_map(|e| e["endedAt"].as_u64().or_else(|| e["startedAt"].as_u64()))
            .max()
            .unwrap_or(invocation_start);
        content["endedAt"] = end.into();
        content["durationMs"] = end.saturating_sub(invocation_start).into();
    }
    content["lane"] = lane(payload["hookEventName"].as_str().unwrap_or("")).into();
    if let Some(call) = payload["toolCallId"].as_str().filter(|s| !s.is_empty()) {
        content["anchorToolCallId"] = call.into();
    }
    content["executions"] = Value::Array(executions);
    Some(content)
}

/// Node `hookBlockErrorDelta`: an executed UserPromptSubmit block becomes the
/// session's `lastError`. `row` is the invocation row after the event.
pub fn block_error(
    kind: Kind,
    payload: &Value,
    row: &Value,
    now: u64,
    trace: &str,
) -> Option<Value> {
    if kind != Kind::Blocked || payload["hookEventName"] != "UserPromptSubmit" {
        return None;
    }
    let run = &payload["hookRunId"];
    let execution = row["executions"]
        .as_array()?
        .iter()
        .find(|e| e["hookRunId"] == *run)?;
    if execution["didExecute"] != true {
        return None;
    }
    let reason = execution["blockReason"]
        .as_str()
        .filter(|r| !r.is_empty())?;
    let diagnostic = ["stderrPreview", "errorMessage", "stdoutPreview"]
        .iter()
        .filter_map(|key| payload[*key].as_str().map(str::trim))
        .find(|value| !value.is_empty() && *value != reason);
    let shown = diagnostic.unwrap_or(reason);
    let message = if shown == PROMPT_BLOCK {
        PROMPT_BLOCK.to_owned()
    } else {
        format!("{PROMPT_BLOCK}: {shown}")
    };
    let mut detail = format!("Hook block reason: {reason}");
    if let Some(diagnostic) = diagnostic {
        detail.push_str(&format!("\nHook error: {diagnostic}"));
    }
    Some(json!({
        "code": "fault.runtime.hookBlocked",
        "message": message,
        "recoverable": false,
        "at": now,
        "source": "runtime",
        "traceId": trace,
        "detail": detail,
        "attribution": {"source": "runtime", "reason": "hook_blocked"},
    }))
}

/// Node `onSessionResumed`: a restarted runtime cannot still be running a
/// hook, so running rows end failed and running executions `cancelled`.
pub fn close_running(row: &mut Value, now: u64) -> bool {
    if row["kind"] != "hookInvocation" || row["state"] != "running" {
        return false;
    }
    if let Some(executions) = row["executions"].as_array_mut() {
        for execution in executions.iter_mut().filter(|e| e["state"] == "running") {
            let started = execution["startedAt"].as_u64().unwrap_or(now);
            execution["state"] = "failed".into();
            execution["outcome"] = "cancelled".into();
            execution["endedAt"] = now.into();
            execution["durationMs"] = now.saturating_sub(started).into();
        }
    }
    let started = row["startedAt"].as_u64().unwrap_or(now);
    row["state"] = "failed".into();
    row["endedAt"] = now.into();
    row["durationMs"] = now.saturating_sub(started).into();
    true
}
