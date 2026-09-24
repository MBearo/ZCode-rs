//! Message-level facts the cold synthesis reads from a stored transcript
//! (Node `transcript-hydration.ts` helpers).
use super::policy::{self, Policy, text_of};
use super::schemas;
use crate::node_history::Record;
use serde_json::{Map, Value, json};

pub fn finite(value: &Value) -> Option<f64> {
    value.as_f64().filter(|n| n.is_finite())
}

/// JS truthiness of a string field: a non-empty string.
pub fn text(value: &Value) -> Option<&str> {
    value.as_str().filter(|s| !s.is_empty())
}

/// Node `messageCreatedAtMs`.
pub fn created_ms(record: &Record) -> Option<f64> {
    finite(&record.info["time"]["created"])
}

/// Node `messageEndAtMs`: completion (or creation) and the latest part end.
pub fn end_ms(record: &Record) -> Option<f64> {
    let time = &record.info["time"];
    let mut end = finite(&time["completed"]).or_else(|| finite(&time["created"]));
    for part in &record.parts {
        let part_end = match part["type"].as_str() {
            Some("reasoning") => {
                finite(&part["time"]["end"]).or_else(|| finite(&part["time"]["start"]))
            }
            Some("tool") if part["state"].get("time").is_some() => {
                let time = &part["state"]["time"];
                finite(&time["end"]).or_else(|| finite(&time["start"]))
            }
            _ => None,
        };
        if let Some(part_end) = part_end {
            end = Some(end.map_or(part_end, |e| e.max(part_end)));
        }
    }
    end
}

/// Node `inputIntentOfMessage`: the full persisted intent, else the legacy seed.
pub fn input_intent(record: &Record) -> Option<Value> {
    let metadata = &record.info["metadata"];
    if let Some(value) = schemas::input_intent(&metadata["conversationInputIntent"]) {
        let mut intent = Map::new();
        for key in ["sourceCommandId", "queueItemId", "clientId", "kind", "text"] {
            intent.insert(key.into(), value[key].clone());
        }
        for key in ["modelSelection", "mode"] {
            if value.get(key).is_some() {
                intent.insert(key.into(), value[key].clone());
            }
        }
        if value.get("planEnabled").is_some() {
            intent.insert("planEnabled".into(), value["planEnabled"].clone());
        }
        intent.insert(
            "admissionSeq".into(),
            value["order"]["admissionSeq"].clone(),
        );
        intent.insert("admittedAt".into(), value["admittedAt"].clone());
        intent.insert(
            "requestedDelivery".into(),
            value["delivery"]["requested"].clone(),
        );
        intent.insert(
            "admittedDelivery".into(),
            value["delivery"]["admitted"].clone(),
        );
        if value["order"].get("queuePosition").is_some() {
            intent.insert(
                "queuePosition".into(),
                value["order"]["queuePosition"].clone(),
            );
        }
        if let Some(code) = text(&value["delivery"]["fallbackReasonCode"]) {
            intent.insert("fallbackReasonCode".into(), code.into());
        }
        if value["attachments"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
        {
            intent.insert("attachmentRefs".into(), value["attachments"].clone());
        }
        if value.get("provenance").is_some() {
            intent.insert("provenance".into(), value["provenance"].clone());
        }
        return Some(Value::Object(intent));
    }
    let legacy = metadata.get("inputIntent").filter(|v| v.is_object())?;
    let delivery = |key: &str, allowed: &[&str]| {
        legacy[key]
            .as_str()
            .is_some_and(|value| allowed.contains(&value))
    };
    let valid = legacy["sourceCommandId"].is_string()
        && legacy["queueItemId"].is_string()
        && legacy["clientId"].is_string()
        && matches!(
            legacy["kind"].as_str(),
            Some("sendText" | "sendGoalCommand")
        )
        && legacy["admissionSeq"].is_number()
        && legacy["admittedAt"].is_number()
        && delivery("requestedDelivery", &["auto", "startNow", "queue", "guide"])
        && delivery("admittedDelivery", &["startNow", "queue", "guide"]);
    valid.then(|| legacy.clone())
}

/// Node `executionKindOfMessage`.
pub fn execution_kind(record: &Record) -> Option<&str> {
    record.info["metadata"]["executionKind"]
        .as_str()
        .filter(|kind| matches!(*kind, "agent" | "controlOnly"))
}

/// Node `epilogueStartOfMessage`: a non-negative integer or absent.
pub fn epilogue_start(record: &Record) -> Option<Value> {
    let value = &record.info["metadata"]["epilogueStart"];
    finite(value)
        .filter(|n| n.fract() == 0.0 && *n >= 0.0)
        .map(|_| value.clone())
}

/// Node `attachmentMetasOfMessage`.
pub fn attachment_metas(parts: &[Value]) -> Vec<Value> {
    parts
        .iter()
        .filter(|part| part["type"] == "file")
        .enumerate()
        .map(|(index, part)| {
            let url = part["url"].as_str().unwrap_or("");
            let stable = !url.is_empty() && !url.starts_with("data:");
            let basename = if stable {
                url.rsplit(['/', '\\']).next().unwrap_or("")
            } else {
                ""
            };
            let file_name = match part["filename"].as_str() {
                Some(name) => name.to_owned(),
                None if !basename.is_empty() => basename.to_owned(),
                None => format!("attachment-{}", index + 1),
            };
            let bytes = match &part["metadata"]["sizeBytes"] {
                Value::Null => json!(0),
                other => other.clone(),
            };
            let mut meta = json!({"fileName": file_name, "mime": part["mime"], "bytes": bytes});
            if stable {
                meta["ref"] = url.into();
            }
            meta
        })
        .collect()
}

fn fork_context_of_metadata(metadata: &Value) -> Option<Value> {
    let context = metadata.get("forkContext").filter(|c| c.is_object())?;
    if context["kind"] != "session_fork" || !context["parentSessionId"].is_string() {
        return None;
    }
    let mut out = json!({"parentSessionId": context["parentSessionId"]});
    if context["restoredFileCount"].is_number() {
        out["restoredFileCount"] = context["restoredFileCount"].clone();
    }
    for key in ["targetCheckpointId", "targetMessageId"] {
        if context[key].is_string() {
            out[key] = context[key].clone();
        }
    }
    Some(out)
}

/// Node `forkContextOfMessage`.
pub fn fork_context(record: &Record) -> Option<Value> {
    for part in &record.parts {
        if part["type"] == "timeline" && part["timelineType"] == "session_fork" {
            let mut out = json!({"parentSessionId": js_string(&part["parentSessionId"])});
            if part["restoredFileCount"].is_number() {
                out["restoredFileCount"] = part["restoredFileCount"].clone();
            }
            if let Some(id) = text(&part["targetCheckpointId"]) {
                out["targetCheckpointId"] = id.into();
            }
            if truthy(&part["targetMessageId"]) {
                out["targetMessageId"] = js_string(&part["targetMessageId"]).into();
            }
            return Some(out);
        }
        if part["type"] == "text"
            && let Some(context) = fork_context_of_metadata(&part["metadata"])
        {
            return Some(context);
        }
    }
    if record.info["role"] == "user" {
        fork_context_of_metadata(&record.info["metadata"])
    } else {
        None
    }
}

/// Node `isForkTimelineMessage`.
pub fn is_fork_timeline(record: &Record) -> bool {
    policy::policy(record) == Policy::TimelineOnly && fork_context(record).is_some()
}

/// JS truthiness.
pub fn truthy(value: &Value) -> bool {
    crate::node_history::branch::truthy(Some(value))
}

/// JS `String(value)` for the scalar ids the transcript carries.
pub fn js_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "null".into(),
        other => crate::js_json::stringify(other),
    }
}

/// Node `workflowLaunchOfMessage`.
pub fn workflow_launch(record: &Record) -> Option<Value> {
    if record.info["role"] != "user" || record.info["source"] != "workflow_launch" {
        return None;
    }
    let metadata = record.info.get("metadata").filter(|m| m.is_object())?;
    schemas::workflow_launch(&metadata["workflowLaunch"])
}

/// Node `steerDeliveryOfMessage`.
pub fn steer_delivery(record: &Record) -> Option<&str> {
    if record.info["role"] != "user" {
        return None;
    }
    record.info["metadata"]["turnSteerDelivery"]
        .as_str()
        .filter(|d| matches!(*d, "guide" | "queue"))
}

/// Node `isTurnBoundaryStarter`.
pub fn is_turn_boundary(record: &Record) -> bool {
    if policy::real_user_starter(record) {
        return steer_delivery(record) != Some("guide");
    }
    workflow_launch(record).is_some() || policy::model_only_trigger(record).is_some()
}

/// Node `backgroundResultOriginMetaOfMessage`.
pub fn background_origin_meta(record: &Record) -> Option<Value> {
    let message = &record.info["metadata"]["originMeta"];
    let candidate = if message.is_null() {
        let part = record.parts.iter().find(|p| p["type"] == "text")?;
        &part["metadata"]["originMeta"]
    } else {
        message
    };
    candidate.as_object()?;
    let source = candidate["backgroundSource"].as_str()?;
    let work_id = candidate["workId"].as_str().map(str::trim).unwrap_or("");
    let title = candidate["title"].as_str().map(str::trim).unwrap_or("");
    if !matches!(source, "bash" | "subagent" | "workflow") || work_id.is_empty() || title.is_empty()
    {
        return None;
    }
    let mut meta = json!({"backgroundSource": source, "title": title, "workId": work_id});
    let notification = &candidate["workflowNotification"];
    if !notification.is_null()
        && let Some(parsed) = schemas::workflow_notification(notification)
    {
        meta["workflowNotification"] = parsed;
    }
    Some(meta)
}

/// Node `isLegacyCompactMaintenanceInput`: an old `/compact` user host without
/// canonical policy, followed by the assistant compaction it triggered.
pub fn is_legacy_compact_input(record: &Record, next: Option<&Record>) -> bool {
    let info = &record.info;
    if info["role"] != "user" || info.get("visibility").is_some() || info.get("semantics").is_some()
    {
        return false;
    }
    let text = text_of(&record.parts);
    let text = text.trim();
    if text != "/compact" && !text.starts_with("/compact ") {
        return false;
    }
    next.is_some_and(|next| {
        next.info["role"] == "assistant"
            && next.parts.iter().any(|p| {
                p["type"] == "compaction"
                    || (p["type"] == "timeline" && p["timelineType"] == "context_compaction")
            })
    })
}

/// Node `isProviderContextOnlyAssistant`.
pub fn is_provider_context_assistant(record: &Record) -> bool {
    record.info["role"] == "assistant" && policy::policy(record) == Policy::ProviderContextOnly
}
