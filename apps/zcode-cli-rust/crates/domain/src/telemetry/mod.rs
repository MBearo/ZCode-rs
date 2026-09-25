//! Content-free telemetry of live session events (spec rust-m9-usage-logs §5):
//! Node `ConversationTelemetryFactNormalizer` (`v4/telemetry/event`) and
//! `mapComputerUseOperationEvent` (`computer-use/operation-event`). The
//! events are Node `SessionEvent`s by their wire type and payload; nothing
//! here reads a transcript, so hydration never produces facts.
mod bounded;
mod facts;
mod tools;

use bounded::{BoundedMap, BoundedSet};
use serde_json::{Map, Value, json};

/// One live Node `SessionEvent`.
#[derive(Clone, Copy, Debug)]
pub struct Event<'a> {
    pub id: &'a str,
    /// The session's event sequence (0 for events mirrored from a child).
    pub seq: u64,
    /// Epoch milliseconds.
    pub at: u64,
    pub session: &'a str,
    pub turn: Option<&'a str>,
    /// The `SessionEventType` wire name (`turn_started`, `tool_call_result`, …).
    pub kind: &'a str,
    pub payload: &'a Value,
}

/// Facts of the session outside the event (Node `runtimeMetadata`).
#[derive(Clone, Debug, Default)]
pub struct Runtime {
    /// The session record's startup memory preference.
    pub memory_enabled: Option<bool>,
    /// The selected `(modelId, providerId)`.
    pub model: Option<(String, String)>,
}

/// The per-process normalizer state (Node keeps one per gateway).
#[derive(Debug, Default)]
pub struct Normalizer {
    first_chunks: BoundedSet,
    command_by_turn: BoundedMap<String>,
    tool_names: BoundedMap<String>,
    /// Session → `(modelId, providerId)` of its latest model request status.
    model_by_session: BoundedMap<(String, String)>,
    /// `session\0querySource` → completed step requests awaiting their usage.
    completed_requests: BoundedMap<Vec<Value>>,
}

/// Node `optionalString`: a non-empty string.
fn text(value: &Value) -> Option<&str> {
    value.as_str().filter(|s| !s.is_empty())
}

/// Node `nonNegative`: a finite number ≥ 0.
fn non_negative(value: &Value) -> Option<f64> {
    value.as_f64().filter(|n| n.is_finite() && *n >= 0.0)
}

/// A JSON number that stays an integer when it is one.
fn number(value: f64) -> Value {
    if value.fract() == 0.0 && value.abs() < 9e15 {
        (value as i64).into()
    } else {
        value.into()
    }
}

/// WHATWG `new URL(baseURL).hostname`, `None` when empty or unparsable.
fn hostname(base_url: &Value) -> Option<String> {
    let url = url::Url::parse(text(base_url)?).ok()?;
    url.host_str().filter(|h| !h.is_empty()).map(str::to_owned)
}

fn turn_key(event: &Event) -> Option<String> {
    event.turn.map(|turn| format!("{}\0{turn}", event.session))
}

impl Normalizer {
    /// Node `factBaseFields` with the kind; `command` is the `sourceCommandId`.
    fn base(
        event: &Event,
        runtime: &Runtime,
        command: Option<&str>,
        kind: &str,
    ) -> Map<String, Value> {
        let mut fact = Map::new();
        if let Some(memory) = runtime.memory_enabled {
            fact.insert("memoryEnabled".into(), memory.into());
        }
        fact.insert("version".into(), 1.into());
        fact.insert("eventId".into(), event.id.into());
        fact.insert("eventSeq".into(), event.seq.into());
        fact.insert("occurredAt".into(), event.at.into());
        fact.insert("sessionId".into(), event.session.into());
        if let Some(command) = command {
            fact.insert("sourceCommandId".into(), command.into());
        }
        if let Some(turn) = event.turn {
            fact.insert("turnId".into(), turn.into());
        }
        fact.insert("kind".into(), kind.into());
        fact
    }

    /// The fact of one live event, if it has one.
    pub fn normalize(&mut self, event: &Event, runtime: &Runtime) -> Option<Value> {
        let key = turn_key(event);
        let command = key
            .as_deref()
            .and_then(|k| self.command_by_turn.get(k))
            .cloned();
        let fact = match event.kind {
            "turn_started" => self.turn_started(event, runtime, key.as_deref()),
            "model_network_status" => self.model_status(event, runtime, command.as_deref()),
            "model_streaming" => self.chunk(event, runtime, command.as_deref()),
            "tool_call_scheduled"
            | "tool_call_started"
            | "tool_call_progress"
            | "tool_call_result"
            | "tool_call_error" => self.tool(event, runtime, (command.as_deref(), key.as_deref())),
            "permission_requested" | "permission_resolved" | "permission_denied" => {
                Some(tools::permission(event, runtime, command.as_deref()))
            }
            "model_complete" => self.usage(event, runtime, command.as_deref()),
            "subagent_spawned" | "subagent_stopped" => {
                tools::subagent(event, runtime, command.as_deref())
            }
            "turn_complete" | "turn_error" => {
                let fact = facts::terminal(event, runtime, command.as_deref());
                self.clear_turn(key.as_deref());
                Some(fact)
            }
            "compact_completed" | "compact_failed" => self.compaction(event, runtime),
            _ => None,
        }?;
        Some(Value::Object(fact))
    }

    fn clear_turn(&mut self, key: Option<&str>) {
        let Some(key) = key else { return };
        self.command_by_turn.remove(key);
        let prefix = format!("{key}\0");
        self.first_chunks.remove_prefix(&prefix);
        self.tool_names.remove_prefix(&prefix);
    }

    /// Node `clearSession`: the session was released.
    pub fn clear_session(&mut self, session: &str) {
        let prefix = format!("{session}\0");
        self.command_by_turn.remove_prefix(&prefix);
        self.first_chunks.remove_prefix(&prefix);
        self.tool_names.remove_prefix(&prefix);
        self.model_by_session.remove(session);
        self.completed_requests.remove_prefix(&prefix);
    }
}

/// Node `mapComputerUseOperationEvent`: the `computer-use/operation-event` of
/// a live event.
pub fn computer_use(event: &Event) -> Option<Value> {
    let payload = event.payload;
    let (kind, needs_turn) = match event.kind {
        "turn_started" => ("turn-started", true),
        "turn_complete" => ("turn-completed", true),
        "turn_error" => ("turn-failed", true),
        "tool_call_scheduled" => ("tool-scheduled", true),
        "tool_call_started" => ("tool-started", false),
        "session_ended" => ("session-closed", false),
        _ => return None,
    };
    if needs_turn && event.turn.is_none() {
        return None;
    }
    let mut out = json!({"eventId": event.id, "sequenceNumber": event.seq,
        "sessionId": event.session, "timestamp": event.at, "kind": kind});
    if kind != "session-closed"
        && let Some(turn) = event.turn
    {
        out["turnId"] = turn.into();
    }
    let call = text(&payload["toolCallId"])
        .map(str::trim)
        .filter(|c| !c.is_empty());
    let name = text(&payload["toolName"])
        .map(str::trim)
        .filter(|n| !n.is_empty());
    match kind {
        "tool-scheduled" => {
            out["toolCallId"] = call?.into();
            out["toolName"] = name?.into();
            let code = payload["input"]["code"].as_str();
            if name == Some("mcp__node_repl__js")
                && code.is_some_and(|c| c.contains("setupComputerUseRuntime"))
            {
                out["computerUse"] = true.into();
            }
        }
        "tool-started" => {
            out["toolCallId"] = call?.into();
            if let Some(name) = name {
                out["toolName"] = name.into();
            }
        }
        _ => {}
    }
    Some(out)
}

#[cfg(test)]
mod tests;
