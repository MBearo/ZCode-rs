//! Compaction parts → compact lifecycle events (Node `transcript-hydration.ts`
//! `synthesizeCompactPart` and its payload builders).
use super::facts::{js_string, truthy};
use super::synth::Synth;
use super::synth_goals::template;
use serde_json::{Value, json};

/// Node `normalizeCompactTimelineStatus`.
fn status(value: &Value) -> Option<&'static str> {
    Some(match value.as_str()? {
        "started" => "started",
        "retrying" => "retrying",
        "skipped" => "skipped",
        "completed" => "completed",
        "failed" => "failed",
        "interrupted" | "cancelled" => "interrupted",
        _ => return None,
    })
}

/// Node `compactEventType`.
fn event_kind(status: &str) -> &'static str {
    match status {
        "started" | "retrying" => "compact_started",
        "completed" | "skipped" => "compact_completed",
        _ => "compact_failed",
    }
}

fn truthy_members(out: &mut Value, part: &Value, keys: &[(&str, &str)]) {
    for (from, to) in keys {
        if let Some(value) = part.get(*from).filter(|v| truthy(v)) {
            out[*to] = value.clone();
        }
    }
}

/// The token, attempt and timing members every compact payload ends with
/// (present unless undefined).
fn trailing_members(out: &mut Value, part: &Value) {
    for key in [
        "preCompactTokenCount",
        "postCompactTokenCount",
        "truePostCompactTokenCount",
        "attempt",
        "maxAttempts",
    ] {
        if let Some(value) = part.get(key) {
            out[key] = value.clone();
        }
    }
    if let Some(start) = part["time"].get("start") {
        out["startedAt"] = start.clone();
    }
    if let Some(end) = part["time"].get("end") {
        out["endedAt"] = end.clone();
    }
}

/// Node `compactPayloadFromTimelinePart`.
fn from_timeline(part: &Value) -> Option<(&'static str, Value)> {
    if part["timelineType"] != "context_compaction" {
        return None;
    }
    let status = status(&part["status"])?;
    let mut out = json!({});
    if let Some(operation) = part.get("operationId") {
        out["operationId"] = operation.clone();
    }
    out["messageId"] = template(part.get("messageID")).into();
    out["partId"] = part["id"].clone();
    out["status"] = status.into();
    for key in ["trigger", "display"] {
        if let Some(value) = part.get(key) {
            out[key] = value.clone();
        }
    }
    truthy_members(
        &mut out,
        part,
        &[
            ("sourceCommandId", "sourceCommandId"),
            ("anchorMessageId", "anchorMessageId"),
            ("anchorTurnId", "anchorTurnId"),
            ("phase", "phase"),
            ("compactReason", "compactReason"),
            ("reason", "reason"),
            ("boundaryId", "boundaryId"),
            ("summaryMessageId", "summaryMessageId"),
        ],
    );
    trailing_members(&mut out, part);
    Some((status, out))
}

/// `part.operationId ?? part.boundaryId ?? "legacy-compact-<id>"`.
pub fn legacy_operation_id(part: &Value) -> Value {
    [&part["operationId"], &part["boundaryId"]]
        .into_iter()
        .find(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| format!("legacy-compact-{}", js_string(&part["id"])).into())
}

/// Node `compactPayloadFromLegacyCompactionPart`.
fn from_compaction(part: &Value) -> Option<(&'static str, Value)> {
    let status = status(&part["timelineStatus"])?;
    let trigger = match &part["trigger"] {
        Value::Null if truthy(&part["auto"]) => "auto".into(),
        Value::Null => "manual".into(),
        other => other.clone(),
    };
    let display = match &part["timelineDisplay"] {
        Value::Null => "separator".into(),
        other => other.clone(),
    };
    let mut out = json!({
        "operationId": legacy_operation_id(part),
        "messageId": template(part.get("messageID")),
        "partId": part["id"],
        "status": status,
        "trigger": trigger,
        "display": display,
    });
    truthy_members(
        &mut out,
        part,
        &[
            ("phase", "phase"),
            ("compactReason", "compactReason"),
            ("reason", "reason"),
            ("boundaryId", "boundaryId"),
            ("summaryMessageId", "summaryMessageId"),
            ("tail_start_id", "tailStartMessageId"),
        ],
    );
    trailing_members(&mut out, part);
    Some((status, out))
}

impl Synth<'_> {
    /// Node `synthesizeCompactPart`: the durable compaction part of the same
    /// operation (the one carrying coverage) wins over the timeline part.
    pub fn compact_part(&mut self, part: &Value, turn: &str) -> bool {
        let compact = match part["type"].as_str() {
            Some("timeline") => from_timeline(part),
            Some("compaction") => from_compaction(part),
            _ => None,
        };
        let Some(mut compact) = compact else {
            return false;
        };
        let operation = template(compact.1.get("operationId"));
        if let Some(durable) = self.durable_compacts.get(&operation).copied() {
            compact = from_compaction(durable).unwrap_or_else(|| {
                let (status, mut payload) = compact;
                truthy_members(
                    &mut payload,
                    durable,
                    &[
                        ("tail_start_id", "tailStartMessageId"),
                        ("boundaryId", "boundaryId"),
                        ("summaryMessageId", "summaryMessageId"),
                    ],
                );
                (status, payload)
            });
        }
        if !self.compacts_emitted.insert(operation) {
            return true;
        }
        self.push(event_kind(compact.0), compact.1, Some(turn), None);
        true
    }
}
