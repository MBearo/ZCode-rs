//! The last outcome of a stored subagent child session (Node
//! `lastChildOutcome`).
use crate::node_history::Record;
use crate::node_history::branch::truthy;
use crate::subagent_query::{cancellation, field, non_empty, object};

#[derive(Default)]
pub(crate) struct Outcome {
    pub ended_at: Option<u64>,
    pub status: Option<&'static str>,
    pub summary: Option<String>,
}

/// Node `lastChildOutcome`.
pub(crate) fn last_outcome(messages: Option<&Vec<Record>>) -> Outcome {
    let Some(last) = messages.and_then(|m| m.iter().rev().find(|m| m.info["role"] == "assistant"))
    else {
        return Outcome::default();
    };
    let info = &last.info;
    let error = info.get("error").filter(|e| e.is_object());
    if error.is_some_and(|e| e["name"] == "StreamRecoveryDiscarded") {
        return Outcome::default();
    }
    let text = last
        .parts
        .iter()
        .filter(|p| p["type"] == "text" && p["ignored"] != true)
        .filter_map(|p| non_empty(&p["text"]))
        .collect::<Vec<_>>()
        .join("\n\n");
    let name = error.and_then(|e| e["name"].as_str());
    let error_summary = error.and_then(|e| {
        field(&object(&e["data"]), &["message", "error", "detail"]).or(name.map(str::to_owned))
    });
    let tools = last.parts.iter().any(|p| p["type"] == "tool");
    let completed = info["time"]["completed"].as_u64().filter(|t| *t != 0);
    let status = match name {
        Some(name) if cancellation(name) => Some("cancelled"),
        Some(_) => Some("failed"),
        None if !tools && (completed.is_some() || truthy(info.get("finish"))) => Some("success"),
        None => None,
    };
    Outcome {
        ended_at: completed,
        status,
        summary: if text.is_empty() {
            error_summary
        } else {
            Some(text)
        },
    }
}
