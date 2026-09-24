//! Node `persistSyntheticUserNoticeForSession`: a runtime notice the model
//! reads (todo reminder, background result, ...) stored as a synthetic,
//! model-only user message.
use super::Op;
use super::records as r;
use crate::session::Session;
use serde_json::{Map, Value, json};

/// Node `syntheticUserNoticeKind`.
fn kind(source: &str) -> &'static str {
    match source {
        "background_task" => "background_notification",
        "fork" => "fork_notice",
        "rewind" => "rewind_notice",
        "subagent" | "subagent_message" => "subagent_notification",
        "todo_reminder" => "todo_reminder",
        "workflow_launch" => "user_prompt",
        "shared_context" => "shared_context",
        _ => "system_reminder",
    }
}

/// Node `runtimeMetadataForSyntheticUserMessageSource`.
pub fn runtime_message(source: &str) -> Value {
    let source = match source {
        "background_task" | "subagent_message" | "shared_context" => "legacy_synthetic",
        "subagent" => "queued_system_notification",
        "todo_reminder" | "goal_state_change" | "plugin_reference" | "selection_side_chat" => {
            source
        }
        "goal-continuation" => "target_continuation",
        _ => "rewind_notice",
    };
    json!({"source": source})
}

/// Node `mapSyntheticSourceToAnchorOrigin`.
fn origin(source: &str) -> &'static str {
    match source {
        "background_task" | "subagent" => "backgroundResult",
        "goal-continuation" => "goalContinuation",
        _ => "synthetic",
    }
}

/// The notice facts beyond the session's own.
pub struct Notice<'a> {
    pub message: String,
    pub part: String,
    pub source: &'a str,
    pub text: &'a str,
    /// Extra structured metadata (`runtimeMessage` overrides the default).
    pub metadata: Option<Value>,
    pub tools: &'a [String],
}

impl Session {
    /// Node `persistSyntheticUserNoticeForSession` (model-only visibility).
    pub fn node_notice(&mut self, now: u64, n: Notice) {
        if !self.node.created {
            return;
        }
        let visibility = "model-only";
        let extra = n
            .metadata
            .and_then(|m| m.as_object().cloned())
            .unwrap_or_default();
        let mut message_metadata: Map<String, Value> = extra.clone();
        message_metadata.shift_remove("runtimeMessage");
        message_metadata.insert("source".into(), n.source.into());
        message_metadata.insert("visibility".into(), visibility.into());
        let mut part_metadata = extra;
        let runtime = part_metadata
            .get("runtimeMessage")
            .cloned()
            .unwrap_or_else(|| runtime_message(n.source));
        part_metadata.insert("runtimeMessage".into(), runtime);
        part_metadata.insert("source".into(), n.source.into());
        part_metadata.insert("visibility".into(), visibility.into());
        let turn = self.node.turn.as_ref().map(|t| t.runtime.clone());
        let mut info = json!({"id": n.message, "sessionID": self.id, "role": "user",
            "time": {"created": now}, "agent": r::AGENT, "metadata": message_metadata});
        if let Some(selection) = self.node_selection() {
            info["modelSelection"] = selection;
        }
        info["semantics"] = json!({"origin": "agent_runtime", "kind": kind(n.source),
            "source": n.source, "uiVisibility": "hidden", "providerVisibility": "visible",
            "transcriptVisibility": "hidden"});
        if let Some(anchor) = r::anchor(turn.as_deref(), Some(origin(n.source)), None) {
            info["anchor"] = anchor;
        }
        info["source"] = n.source.into();
        info["synthetic"] = true.into();
        let tools: Map<String, Value> = n.tools.iter().map(|t| (t.clone(), true.into())).collect();
        info["tools"] = Value::Object(tools);
        info["visibility"] = visibility.into();
        let part = json!({"id": n.part, "sessionID": self.id, "messageID": n.message,
            "type": "text", "text": n.text, "synthetic": true, "time": {"start": now, "end": now},
            "metadata": part_metadata});
        if let Some(turn) = self.node.turn.as_mut() {
            turn.messages.push(n.message.clone());
        }
        self.node.push(now, Op::Message(info));
        self.node.push(now, Op::Part(part));
    }
}
