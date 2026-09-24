//! Row decoding of the shared Node database (Node `session-store/codecs.ts`).
//! Spec rust-m11-node-storage §2.3.
use serde_json::{Map, Value, json};

pub const MODEL_SELECTION_ENTRY: &str = "runtime/model_selection";

/// Node `SESSION_TASK_TYPES`.
pub const TASK_TYPES: [&str; 7] = [
    "interactive",
    "fork",
    "selection_side_chat",
    "workflow_parent",
    "workflow_child",
    "subagent_child",
    "nested_workflow_child",
];

/// Node `SESSION_TITLE_SOURCES`.
pub const TITLE_SOURCES: [&str; 4] = ["default", "first_input", "generated", "custom"];

/// JS `String.prototype.trim` over a non-empty result (zod `.trim().min(1)`).
fn trimmed(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(|s| s.trim_matches(super::migrations::js_whitespace))
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Node `parseModelSelectionValue` (`modelSelectionSchema`, strict, trimmed).
pub fn parse_model_selection(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    if object
        .keys()
        .any(|k| !matches!(k.as_str(), "providerId" | "modelId" | "options"))
    {
        return None;
    }
    let provider = trimmed(object.get("providerId")?)?;
    let model = trimmed(object.get("modelId")?)?;
    let mut selection = json!({"providerId": provider, "modelId": model});
    match object.get("options") {
        None => {}
        Some(Value::Object(options)) => {
            if options.keys().any(|k| k != "reasoningLevel") {
                return None;
            }
            let mut parsed = Map::new();
            if let Some(level) = options.get("reasoningLevel") {
                parsed.insert("reasoningLevel".into(), trimmed(level)?.into());
            }
            selection["options"] = Value::Object(parsed);
        }
        Some(_) => return None,
    }
    Some(selection)
}

/// Node `saveMessage`'s legacy `model` member of user messages.
pub fn legacy_user_model(selection: Option<&Value>) -> Value {
    let Some(selection) = selection.filter(|s| s.is_object()) else {
        return json!({});
    };
    let mut model = json!({"providerID": selection["providerId"], "modelID": selection["modelId"]});
    if let Some(level) = selection["options"]["reasoningLevel"]
        .as_str()
        .filter(|l| !l.is_empty())
    {
        model["variant"] = level.into();
    }
    model
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// Node `decodeMessageRow`: the stored JSON without legacy members, then
/// `id` and `sessionID` from the columns (appended, like the JS spread).
pub fn decode_message(data: &str, id: &str, session_id: &str) -> serde_json::Result<Value> {
    let mut message = object(serde_json::from_str(data)?);
    match message.get("role").and_then(Value::as_str) {
        Some("user") => {
            message.shift_remove("model");
            let selection = message
                .shift_remove("modelSelection")
                .and_then(|raw| parse_model_selection(&raw));
            if let Some(selection) = selection {
                message.insert("modelSelection".into(), selection);
            }
        }
        Some("assistant") => {
            for key in ["providerID", "modelID", "variant"] {
                message.shift_remove(key);
            }
        }
        _ => {}
    }
    message.insert("id".into(), id.into());
    message.insert("sessionID".into(), session_id.into());
    Ok(Value::Object(message))
}

/// Node `decodeTimelineSelection`: a strict selection plus an optional string label.
fn timeline_selection(value: Option<Value>) -> Option<Value> {
    let mut raw = object(value.filter(Value::is_object)?);
    let label = raw.shift_remove("label");
    let mut selection = parse_model_selection(&Value::Object(raw))?;
    if let Some(label) = label.filter(Value::is_string) {
        selection["label"] = label;
    }
    Some(selection)
}

/// Node `decodePartRow`.
pub fn decode_part(
    data: &str,
    id: &str,
    session_id: &str,
    message_id: &str,
) -> serde_json::Result<Value> {
    let mut part = object(serde_json::from_str(data)?);
    let kind = part.get("type").and_then(Value::as_str).map(str::to_owned);
    if kind.as_deref() == Some("timeline")
        && part.get("timelineType").and_then(Value::as_str) == Some("model_change")
    {
        part.shift_remove("fromModel");
        part.shift_remove("toModel");
        let from = timeline_selection(part.shift_remove("fromModelSelection"));
        let to = timeline_selection(part.shift_remove("toModelSelection"));
        if let Some(from) = from {
            part.insert("fromModel".into(), from);
        }
        if let Some(to) = to {
            part.insert("toModel".into(), to);
        }
    } else if kind.as_deref() == Some("subtask") {
        part.shift_remove("model");
        let model = part
            .shift_remove("modelSelection")
            .and_then(|raw| parse_model_selection(&raw));
        if let Some(model) = model {
            part.insert("model".into(), model);
        }
    }
    part.insert("id".into(), id.into());
    part.insert("sessionID".into(), session_id.into());
    part.insert("messageID".into(), message_id.into());
    Ok(Value::Object(part))
}

/// Node `decodeSessionEntryRow`'s data: model selections are unwrapped.
pub fn decode_entry_data(kind: &str, data: &str) -> serde_json::Result<Value> {
    let raw: Value = serde_json::from_str(data)?;
    if kind != MODEL_SELECTION_ENTRY {
        return Ok(raw);
    }
    let Some(stored) = raw.get("modelSelection").filter(|_| raw.is_object()) else {
        // 非对象或缺失成员对应 JS 的 undefined。
        return Ok(Value::Null);
    };
    Ok(parse_model_selection(stored).unwrap_or_else(|| stored.clone()))
}

/// Node `partCreatedAt`.
pub fn part_created_at(part: &Value, fallback: i64) -> i64 {
    let start = |v: &Value| v.as_i64();
    match part["type"].as_str() {
        Some("text" | "reasoning" | "compaction" | "timeline") => {
            start(&part["time"]["start"]).unwrap_or(fallback)
        }
        Some("tool") => match part["state"]["status"].as_str() {
            Some("running" | "completed" | "error") => {
                start(&part["state"]["time"]["start"]).unwrap_or(fallback)
            }
            _ => fallback,
        },
        Some("retry") => start(&part["time"]["created"]).unwrap_or(fallback),
        _ => fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_selections_are_strict_and_trimmed() {
        let ok = json!({"providerId":" p ","modelId":"m","options":{"reasoningLevel":" high "}});
        assert_eq!(
            parse_model_selection(&ok),
            Some(json!({"providerId":"p","modelId":"m","options":{"reasoningLevel":"high"}}))
        );
        assert_eq!(
            parse_model_selection(&json!({"providerId":"p","modelId":"m","x":1})),
            None
        );
        assert_eq!(
            parse_model_selection(&json!({"providerId":"p","modelId":" "})),
            None
        );
        assert_eq!(
            parse_model_selection(&json!({"providerId":"p","modelId":"m","options":{}})),
            Some(json!({"providerId":"p","modelId":"m","options":{}}))
        );
        assert_eq!(legacy_user_model(None), json!({}));
        assert_eq!(
            legacy_user_model(Some(
                &json!({"providerId":"p","modelId":"m","options":{"reasoningLevel":"max"}})
            )),
            json!({"providerID":"p","modelID":"m","variant":"max"})
        );
    }

    #[test]
    fn decoding_drops_legacy_members_like_node() {
        let user = r#"{"role":"user","time":{"created":1},"modelSelection":{"providerId":"p","modelId":"m"},"agent":"a","model":{"providerID":"p"}}"#;
        let decoded = decode_message(user, "msg_1", "sess_1").unwrap();
        let keys: Vec<&String> = decoded.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            ["role", "time", "agent", "modelSelection", "id", "sessionID"]
        );
        let assistant = r#"{"role":"assistant","providerID":"p","modelId":"m","variant":"x"}"#;
        assert_eq!(
            decode_message(assistant, "a", "s").unwrap(),
            json!({"role":"assistant","modelId":"m","id":"a","sessionID":"s"})
        );
        let change = r#"{"timelineType":"model_change","type":"timeline","toModel":{"providerID":"p"},"fromModelSelection":{"providerId":"p","modelId":"m","label":"M"},"toModelSelection":{"providerId":"q","modelId":"n","bad":1}}"#;
        assert_eq!(
            decode_part(change, "p1", "s", "m1").unwrap(),
            json!({"timelineType":"model_change","type":"timeline","fromModel":{"providerId":"p","modelId":"m","label":"M"},"id":"p1","sessionID":"s","messageID":"m1"})
        );
        assert_eq!(
            decode_entry_data(
                MODEL_SELECTION_ENTRY,
                r#"{"modelSelection":null,"providerId":"old"}"#
            )
            .unwrap(),
            Value::Null
        );
        assert_eq!(
            part_created_at(&json!({"type":"tool","state":{"status":"pending"}}), 7),
            7
        );
        assert_eq!(
            part_created_at(
                &json!({"type":"tool","state":{"status":"running","time":{"start":3}}}),
                7
            ),
            3
        );
    }
}
