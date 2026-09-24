//! Node timeline records (`persistAssistantTimelinePartForSession`) and the
//! compaction timeline, summary and reminder records (`compact-persistence.ts`).
//! Pure builders shared by the journal and the store's compaction commit.
use super::records::{self as r, AGENT};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// The session facts a timeline host assistant records.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Host {
    pub session: String,
    pub provider: String,
    pub model: String,
    pub mode: String,
    pub plan: bool,
    pub cwd: String,
}

/// Node `persistAssistantTimelinePartForSession`: the timeline host
/// assistant and its `timeline` part (`draft` in Node's key order).
pub fn timeline_records(
    host: &Host,
    (message, part): (&str, &str),
    parent: &str,
    (created, completed): (u64, Option<u64>),
    draft: Value,
) -> (Value, Value) {
    let mut time = json!({"created": created});
    if let Some(completed) = completed {
        time["completed"] = completed.into();
    }
    let mut info = json!({"id": message, "sessionID": host.session, "role": "assistant",
        "time": time, "parentID": parent});
    if !host.model.is_empty() && !host.provider.is_empty() {
        info["modelId"] = host.model.clone().into();
        info["providerId"] = host.provider.clone().into();
    }
    info["mode"] = host.mode.clone().into();
    info["planEnabled"] = host.plan.into();
    info["agent"] = AGENT.into();
    info["path"] = json!({"cwd": host.cwd, "root": host.cwd});
    info["cost"] = 0.into();
    info["tokens"] = r::tokens(None);
    info["finish"] = draft["status"].clone();
    info["semantics"] = json!({"origin": "system", "kind": "timeline_event", "uiVisibility": "visible",
        "providerVisibility": "hidden", "transcriptVisibility": "visible"});
    let mut timeline = draft;
    timeline["id"] = part.into();
    timeline["sessionID"] = host.session.clone().into();
    timeline["messageID"] = message.into();
    timeline["type"] = "timeline".into();
    (info, timeline)
}

/// Node `CompactTimelineContext` of the running compaction.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Compaction {
    pub operation: String,
    pub message: String,
    pub part: String,
    pub trigger: String,
    pub phase: String,
    pub reason: String,
    pub source_command: Option<String>,
    pub started: u64,
    pub pre_tokens: Option<u64>,
    /// The host assistant's parent (Node `latestConversationMessageId`).
    pub parent: String,
    pub max_attempts: u32,
    /// The manual compaction carried custom instructions.
    #[serde(default)]
    pub custom_instructions: bool,
}

/// Node `defaultCompactPhaseForTrigger` / `defaultCompactReasonForTrigger`.
pub fn defaults(trigger: &str) -> (&'static str, &'static str) {
    match trigger {
        "auto" => ("pre_request", "context_limit"),
        "reactive" => ("reactive", "provider_overflow"),
        "session_memory" => ("standalone_turn", "context_limit"),
        _ => ("standalone_turn", "user_requested"),
    }
}

impl Compaction {
    /// Node `buildCompactTimelinePayload`; `update` holds the optional members.
    pub fn payload(&self, status: &str, update: &Value) -> Value {
        let mut out = json!({"operationId": self.operation, "messageId": self.message,
            "partId": self.part, "status": status, "trigger": self.trigger, "phase": self.phase,
            "compactReason": self.reason, "display": "separator"});
        if let Some(tokens) = self.pre_tokens {
            out["preCompactTokenCount"] = tokens.into();
        }
        if let Some(command) = &self.source_command {
            out["sourceCommandId"] = command.clone().into();
        }
        out["startedAt"] = self.started.into();
        for key in [
            "replace",
            "reason",
            "attempt",
            "maxAttempts",
            "boundaryId",
            "summaryMessageId",
            "tailStartMessageId",
            "postCompactTokenCount",
            "truePostCompactTokenCount",
            "endedAt",
        ] {
            if let Some(value) = update.get(key).filter(|v| !v.is_null()) {
                out[key] = value.clone();
            }
        }
        out
    }

    /// Node `persistCompactTimeline`: the host, its timeline part and the
    /// compaction part of `payload`.
    pub fn records(&self, host: &Host, payload: &Value) -> Vec<Value> {
        let copy = |out: &mut Map<String, Value>, key: &str, to: &str| {
            if let Some(value) = payload.get(key).filter(|v| !v.is_null()) {
                out.insert(to.into(), value.clone());
            }
        };
        let mut time = json!({"start": payload["startedAt"]});
        if let Some(end) = payload.get("endedAt") {
            time["end"] = end.clone();
        }
        let mut draft = Map::new();
        draft.insert("timelineType".into(), "context_compaction".into());
        for (key, to) in [
            ("display", "display"),
            ("status", "status"),
            ("operationId", "operationId"),
            ("sourceCommandId", "sourceCommandId"),
            ("trigger", "trigger"),
            ("phase", "phase"),
            ("compactReason", "compactReason"),
            ("boundaryId", "boundaryId"),
            ("summaryMessageId", "summaryMessageId"),
            ("preCompactTokenCount", "preCompactTokenCount"),
            ("postCompactTokenCount", "postCompactTokenCount"),
            ("truePostCompactTokenCount", "truePostCompactTokenCount"),
            ("attempt", "attempt"),
            ("maxAttempts", "maxAttempts"),
            ("reason", "reason"),
        ] {
            copy(&mut draft, key, to);
        }
        draft.insert("time".into(), time.clone());
        let timeline_part = format!("part_{}_timeline", self.part);
        let (info, timeline) = timeline_records(
            host,
            (&self.message, &timeline_part),
            &self.parent,
            (self.started, payload["endedAt"].as_u64()),
            Value::Object(draft),
        );
        let mut part = Map::new();
        for (key, value) in [
            ("id", json!(self.part)),
            ("sessionID", json!(host.session)),
            ("messageID", json!(self.message)),
            ("type", json!("compaction")),
            ("auto", json!(self.trigger == "auto")),
            ("trigger", json!(self.trigger)),
            ("phase", json!(self.phase)),
            ("compactReason", json!(self.reason)),
            ("operationId", json!(self.operation)),
            ("timelineStatus", payload["status"].clone()),
            ("timelineDisplay", json!("separator")),
        ] {
            part.insert(key.into(), value);
        }
        for (key, to) in [
            ("replace", "replace"),
            ("reason", "reason"),
            ("attempt", "attempt"),
            ("maxAttempts", "maxAttempts"),
            ("boundaryId", "boundaryId"),
            ("summaryMessageId", "summaryMessageId"),
            ("tailStartMessageId", "tail_start_id"),
            ("preCompactTokenCount", "preCompactTokenCount"),
            ("postCompactTokenCount", "postCompactTokenCount"),
            ("truePostCompactTokenCount", "truePostCompactTokenCount"),
        ] {
            copy(&mut part, key, to);
        }
        part.insert("time".into(), time);
        vec![info, timeline, Value::Object(part)]
    }
}

/// Node `persistCompactSummary`'s message and parts.
pub fn summary_records(
    host: &Host,
    ids: (&str, &str, &str),
    now: u64,
    (content, body): (&str, &str),
    selection: &Value,
    tools: &Value,
    (boundary, operation): (&Value, &str),
) -> (Value, Vec<Value>) {
    let (message, text, compaction) = ids;
    let mut info = json!({"id": message, "sessionID": host.session, "role": "user", "time": {"created": now},
        "summary": {"title": "Compact summary", "body": body, "diffs": []}, "agent": AGENT});
    if selection.is_object() {
        info["modelSelection"] = selection.clone();
    }
    info["semantics"] = json!({"origin": "agent_runtime", "kind": "compact_summary", "uiVisibility": "hidden",
        "providerVisibility": "visible", "transcriptVisibility": "hidden"});
    info["tools"] = tools.clone();
    let text = json!({"id": text, "sessionID": host.session, "messageID": message, "type": "text",
        "text": content, "synthetic": true, "time": {"start": now, "end": now}});
    let mut part = json!({"id": compaction, "sessionID": host.session, "messageID": message,
        "type": "compaction", "auto": boundary["trigger"] == "auto", "trigger": boundary["trigger"],
        "phase": boundary["phase"], "compactReason": boundary["compactReason"]});
    if let Some(tail) = boundary
        .get("lastSummarizedMessageId")
        .filter(|v| !v.is_null())
    {
        part["tail_start_id"] = tail.clone();
    }
    part["compactBoundary"] = boundary.clone();
    part["operationId"] = operation.into();
    (info, vec![text, part])
}

/// Node `persistCompactReminderMessage` of a post-compact reminder.
pub fn reminder_records(
    host: &Host,
    (message, part): (&str, &str),
    now: u64,
    (source, content): (&str, &str),
    selection: &Value,
    tools: &Value,
) -> (Value, Vec<Value>) {
    let metadata =
        json!({"runtimeMessage": {"source": source}, "source": source, "visibility": "model-only"});
    let mut info = json!({"id": message, "sessionID": host.session, "role": "user", "time": {"created": now},
        "agent": AGENT, "metadata": metadata});
    if selection.is_object() {
        info["modelSelection"] = selection.clone();
    }
    info["semantics"] = json!({"origin": "agent_runtime", "kind": "system_reminder", "source": source,
        "uiVisibility": "hidden", "providerVisibility": "visible", "transcriptVisibility": "hidden"});
    info["synthetic"] = true.into();
    info["tools"] = tools.clone();
    info["visibility"] = "model-only".into();
    let text = json!({"id": part, "sessionID": host.session, "messageID": message, "type": "text",
        "text": content, "synthetic": true, "time": {"start": now, "end": now}, "metadata": metadata});
    (info, vec![text])
}
