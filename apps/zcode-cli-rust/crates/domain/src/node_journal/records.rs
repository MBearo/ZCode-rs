//! Node record templates (Node `message-persistence.ts`, `turn-model-step.ts`,
//! `turn-tools.ts`, `turn-stop.ts`, `events.ts`), keyed in Node's object
//! literal order so the stored JSON matches Node byte for byte.
use serde_json::{Map, Value, json};

pub const AGENT: &str = "zcode-agent";
pub const MODEL_SELECTION_ENTRY: &str = "runtime/model_selection";
pub const EXECUTION_STATE_ENTRY: &str = "runtime/execution_state";

/// Node `titleFromInput`.
pub fn title_from_input(input: &str) -> String {
    let compact = input.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        return "Untitled session".into();
    }
    let units: Vec<u16> = compact.encode_utf16().collect();
    if units.len() <= 60 {
        compact
    } else {
        format!("{}...", String::from_utf16_lossy(&units[..57]))
    }
}

/// Node `toTokenUsageInfo` over normalized usage (`inputTokens`, ...).
pub fn tokens(usage: Option<&Value>) -> Value {
    let field = |key: &str| {
        usage.map_or(json!(0), |u| {
            u.get(key)
                .filter(|v| !v.is_null())
                .cloned()
                .unwrap_or(json!(0))
        })
    };
    let mut out = Map::new();
    if let Some(total) = usage
        .and_then(|u| u.get("totalTokens"))
        .filter(|v| !v.is_null())
    {
        out.insert("total".into(), total.clone());
    }
    out.insert("input".into(), field("inputTokens"));
    out.insert("output".into(), field("outputTokens"));
    out.insert("reasoning".into(), field("reasoningTokens"));
    out.insert(
        "cache".into(),
        json!({"read": field("cacheReadTokens"), "write": field("cacheWriteTokens")}),
    );
    Value::Object(out)
}

/// Node `buildProjectionAnchor`.
pub fn anchor(turn: Option<&str>, origin: Option<&str>, command: Option<&str>) -> Option<Value> {
    if turn.is_none() && origin.is_none() && command.is_none() {
        return None;
    }
    let mut out = Map::new();
    if let Some(turn) = turn.filter(|t| !t.is_empty()) {
        out.insert("turnId".into(), turn.into());
    }
    if let Some(origin) = origin {
        out.insert("origin".into(), origin.into());
    }
    if let Some(command) = command.filter(|c| !c.is_empty()) {
        out.insert("sourceCommandId".into(), command.into());
    }
    Some(Value::Object(out))
}

/// A session entry (`SessionEntryInfo`).
pub fn entry(id: String, session: &str, kind: &str, now: u64, data: Value, touch: bool) -> Value {
    let mut out = json!({"id": id, "sessionID": session, "type": kind});
    if !touch {
        out["touchSession"] = false.into();
    }
    out["time"] = json!({"created": now, "updated": now});
    out["data"] = data;
    out
}

pub fn model_selection_entry(session: &str, now: u64, selection: Value) -> Value {
    entry(
        format!("{session}:runtime-model-selection"),
        session,
        MODEL_SELECTION_ENTRY,
        now,
        selection,
        false,
    )
}

pub fn execution_state_entry(session: &str, now: u64, mode: &str, plan: bool) -> Value {
    entry(
        format!("{session}:runtime-execution-state"),
        session,
        EXECUTION_STATE_ENTRY,
        now,
        json!({"mode": mode, "planEnabled": plan}),
        false,
    )
}

const USER_SEMANTICS: [(&str, &str); 5] = [
    ("origin", "real_user"),
    ("kind", "user_prompt"),
    ("uiVisibility", "visible"),
    ("providerVisibility", "visible"),
    ("transcriptVisibility", "visible"),
];

const ASSISTANT_SEMANTICS: [(&str, &str); 5] = [
    ("origin", "agent_runtime"),
    ("kind", "assistant_response"),
    ("uiVisibility", "visible"),
    ("providerVisibility", "visible"),
    ("transcriptVisibility", "visible"),
];

fn semantics(fields: [(&str, &str); 5]) -> Value {
    Value::Object(
        fields
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect(),
    )
}

/// The user prompt facts `persistUserPrompt` records.
pub struct UserPrompt<'a> {
    pub id: &'a str,
    /// Node `config.agentName` (`zcode-<type>` for a subagent child).
    pub agent: &'a str,
    pub session: &'a str,
    pub created: u64,
    pub selection: Option<Value>,
    pub turn: &'a str,
    pub command: Option<&'a str>,
    pub tools: &'a [String],
    pub metadata: Option<Value>,
}

/// Node `persistUserPrompt`'s message.
pub fn user_message(p: &UserPrompt) -> Value {
    let mut out = json!({"id": p.id, "sessionID": p.session, "role": "user",
        "time": {"created": p.created}, "agent": p.agent});
    if let Some(selection) = &p.selection {
        out["modelSelection"] = selection.clone();
    }
    out["semantics"] = semantics(USER_SEMANTICS);
    if let Some(anchor) = anchor(Some(p.turn), Some("realUser"), p.command) {
        out["anchor"] = anchor;
    }
    let tools: Map<String, Value> = p.tools.iter().map(|t| (t.clone(), true.into())).collect();
    out["tools"] = Value::Object(tools);
    if let Some(metadata) = &p.metadata {
        out["metadata"] = metadata.clone();
    }
    out
}

/// A text part with its timing.
pub fn text_part(
    id: String,
    session: &str,
    message: &str,
    text: &str,
    start: u64,
    end: u64,
) -> Value {
    json!({"id": id, "sessionID": session, "messageID": message, "type": "text", "text": text,
        "time": {"start": start, "end": end}})
}

/// A reasoning part; `metadata` is the block's provider options.
pub fn reasoning_part(
    id: String,
    session: &str,
    message: &str,
    text: &str,
    metadata: Option<Value>,
    (start, end): (u64, u64),
) -> Value {
    let mut out = json!({"id": id, "sessionID": session, "messageID": message,
        "type": "reasoning", "text": text});
    if let Some(metadata) = metadata {
        out["metadata"] = metadata;
    }
    out["time"] = json!({"start": start, "end": end});
    out
}

pub fn step_start_part(id: String, session: &str, message: &str) -> Value {
    json!({"id": id, "sessionID": session, "messageID": message, "type": "step-start"})
}

pub fn step_finish_part(
    id: String,
    session: &str,
    message: &str,
    reason: &Value,
    tokens: Value,
) -> Value {
    let mut out =
        json!({"id": id, "sessionID": session, "messageID": message, "type": "step-finish"});
    if !reason.is_null() {
        out["reason"] = reason.clone();
    }
    out["cost"] = 0.into();
    out["tokens"] = tokens;
    out
}

/// The assistant facts `persistAssistantMessage` records.
pub struct Assistant<'a> {
    pub id: &'a str,
    pub agent: &'a str,
    pub session: &'a str,
    pub parent: &'a str,
    pub created: u64,
    pub completed: Option<u64>,
    pub error: Option<Value>,
    pub provider: &'a str,
    pub model: &'a str,
    pub mode: &'a str,
    pub plan: bool,
    pub cwd: &'a str,
    pub root: &'a str,
    pub tokens: Option<Value>,
    pub finish: Option<Value>,
    pub turn: &'a str,
}

/// Node `persistAssistantMessage`'s message.
pub fn assistant_message(a: &Assistant) -> Value {
    let mut time = json!({ "created": a.created });
    if let Some(completed) = a.completed {
        time["completed"] = completed.into();
    }
    let mut out = json!({"id": a.id, "sessionID": a.session, "role": "assistant", "time": time});
    if let Some(error) = &a.error {
        out["error"] = error.clone();
    }
    out["parentID"] = a.parent.into();
    for (key, value) in [("modelId", a.model), ("providerId", a.provider)] {
        if !value.is_empty() {
            out[key] = value.into();
        }
    }
    out["mode"] = a.mode.into();
    out["planEnabled"] = a.plan.into();
    out["agent"] = a.agent.into();
    out["path"] = json!({"cwd": a.cwd, "root": a.root});
    out["cost"] = 0.into();
    out["tokens"] = a.tokens.clone().unwrap_or_else(|| tokens(None));
    if let Some(finish) = a.finish.as_ref().filter(|f| !f.is_null()) {
        out["finish"] = finish.clone();
    }
    out["semantics"] = semantics(ASSISTANT_SEMANTICS);
    if let Some(anchor) = anchor(Some(a.turn), None, None) {
        out["anchor"] = anchor;
    }
    out
}

/// A tool part in one of its states; only the pending part carries part
/// metadata (Node spreads the projected name metadata, empty for real names).
pub fn tool_part(
    id: &str,
    session: &str,
    message: &str,
    (call, index, tool): (&str, usize, &str),
    metadata: Option<Value>,
    state: Value,
) -> Value {
    let mut out = json!({"id": id, "sessionID": session, "messageID": message, "type": "tool",
        "callID": call, "declarationIndex": index, "tool": tool});
    if let Some(metadata) = metadata {
        out["metadata"] = metadata;
    }
    out["state"] = state;
    out
}

/// `pending`: the declared call before execution (`persistPendingToolPart`).
pub fn pending_tool(tool: &str, input: &Value) -> Value {
    let raw = crate::js_json::stringify(&json!({"tool": tool, "input": input}));
    json!({"status": "pending", "input": input, "raw": raw})
}

pub fn running_tool(tool: &str, input: &Value, start: u64) -> Value {
    json!({"status": "running", "input": input, "title": tool, "metadata": {}, "time": {"start": start}})
}

pub fn completed_tool(
    tool: &str,
    input: &Value,
    output: &Value,
    display: Option<&Value>,
    (start, end): (u64, u64),
) -> Value {
    let mut metadata = json!({"schemaVersion": 1});
    if let Some(display) = display {
        metadata["display"] = display.clone();
    }
    json!({"status": "completed", "input": input, "output": output, "title": tool,
        "metadata": metadata, "time": {"start": start, "end": end}})
}

pub fn error_tool(
    input: &Value,
    error: &Value,
    model_content: Option<&Value>,
    (start, end): (u64, u64),
) -> Value {
    let mut metadata = json!({});
    if let Some(content) = model_content.filter(|c| c.is_string()) {
        metadata["modelContent"] = content.clone();
    }
    json!({"status": "error", "input": input, "error": error, "metadata": metadata,
        "time": {"start": start, "end": end}})
}
