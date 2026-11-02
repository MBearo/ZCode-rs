// 复制自 apps/zcode-cli-rust/crates/state/src/node/codecs.rs（feat/rs 79b6691c55）。
//! Node 会话库的行解码（Node `session-store/codecs.ts`，docs/specs/rust-m11-node-storage.md §2.3）。
use serde_json::{Map, Value, json};

pub const MODEL_SELECTION_ENTRY: &str = "runtime/model_selection";

/// Node `SESSION_TASK_TYPES`。
pub const TASK_TYPES: [&str; 7] = [
    "interactive",
    "fork",
    "selection_side_chat",
    "workflow_parent",
    "workflow_child",
    "subagent_child",
    "nested_workflow_child",
];

/// JS `String.prototype.trim` 后非空（zod `.trim().min(1)`）。
fn trimmed(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(rust_cli_base::js::string::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Node `parseModelSelectionValue`（`modelSelectionSchema`：strict，去首尾空白）。
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

/// Node `saveMessage` 给 user 消息补的 legacy `model` 成员。
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

/// Node `decodeMessageRow`：去掉 legacy 成员的存储 JSON，再追加列上的 `id` 与 `sessionID`
/// （与 JS 展开的顺序相同）。
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

/// Node `decodeTimelineSelection`：严格的模型选择，外加可选的字符串 label。
fn timeline_selection(value: Option<Value>) -> Option<Value> {
    let mut raw = object(value.filter(Value::is_object)?);
    let label = raw.shift_remove("label");
    let mut selection = parse_model_selection(&Value::Object(raw))?;
    if let Some(label) = label.filter(Value::is_string) {
        selection["label"] = label;
    }
    Some(selection)
}

/// Node `decodePartRow`。
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

/// Node `decodeSessionEntryRow` 的 data：模型选择拆掉 `{modelSelection}` 外壳。
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

/// Node `partCreatedAt`。
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
#[path = "codecs_tests.rs"]
mod tests;
