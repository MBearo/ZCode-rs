//! Node `conversation-message-projection-policy.ts`: which stored messages
//! are real user input, visible assistant output, model-only context,
//! timeline facts or hidden synthetic carriers.
use crate::node_history::Record;
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    RealUserInput,
    VisibleAssistant,
    ProviderContextOnly,
    TimelineOnly,
    HiddenSynthetic,
}

const MODEL_ONLY: &str = "model-only";
const FORK: &str = "fork";
const GOAL_CONTINUATION_PREFIX: &str = "<system-reminder source=\"goal-continuation\">";
const GOAL_CONTINUATION_MARKER: &str = "Continue working toward the active session goal.";
const GOAL_STATE_MARKER: &str = "Current session goal state";
const REWIND_MARKERS: [&str; 2] = ["Conversation rewind applied.", "Workspace rewind applied."];

const PROVIDER_CONTEXT_SOURCES: [&str; 17] = [
    "agent_control_message",
    "background_task",
    "goal-continuation",
    "goal_completion_verification",
    "goal_state_change",
    "plugin_reference",
    "queued_system_notification",
    "resume_goal_state",
    "resume_referenced_session_context",
    "rewind",
    "selection_side_chat",
    "subagent",
    "subagent_message",
    "target_continuation",
    "task_notification",
    "task_status",
    "todo_reminder",
];

const MODEL_ONLY_TRIGGER_SOURCES: [&str; 6] = [
    "background_task",
    "task_notification",
    "subagent",
    "subagent_message",
    "goal-continuation",
    "target_continuation",
];

pub(crate) fn text_of(parts: &[Value]) -> String {
    parts
        .iter()
        .filter(|p| p["type"] == "text" && p["ignored"] != true)
        .filter_map(|p| p["text"].as_str())
        .collect::<Vec<_>>()
        .join("")
}

fn non_empty(value: &Value) -> Option<&str> {
    value.as_str().filter(|s| !s.is_empty())
}

/// Node `messageSource`.
pub fn source(record: &Record) -> Option<&str> {
    let info = &record.info;
    // Node `??` keeps an empty string source; only null/undefined falls through.
    info["source"]
        .as_str()
        .or_else(|| non_empty(&info["metadata"]["source"]))
        .or_else(|| info["semantics"]["source"].as_str())
        .or_else(|| {
            record
                .parts
                .iter()
                .find_map(|p| non_empty(&p["metadata"]["source"]))
        })
}

fn has_model_only_part(parts: &[Value]) -> bool {
    parts.iter().any(|p| {
        p["metadata"]["visibility"] == MODEL_ONLY || p["metadata"]["source"] == "goal-continuation"
    })
}

fn timeline_only(record: &Record) -> bool {
    let info = &record.info;
    if info["semantics"]["kind"] == "timeline_event" {
        return true;
    }
    if info["source"] == FORK || info["metadata"]["source"] == FORK {
        return true;
    }
    record.parts.iter().any(|p| {
        let metadata = &p["metadata"];
        p["type"] == "timeline"
            || metadata["forkContext"]["kind"] == "session_fork"
            || (p["type"] == "compaction"
                && (metadata["timelineStatus"].is_string() || p["summaryMessageId"].is_string()))
    })
}

fn legacy_reminder_text(parts: &[Value]) -> bool {
    let text = text_of(parts);
    let text = text.trim_start();
    text.starts_with(GOAL_CONTINUATION_PREFIX)
        || (text.starts_with("<system-reminder>")
            && (text.contains(GOAL_CONTINUATION_MARKER) || text.contains(GOAL_STATE_MARKER)))
        || REWIND_MARKERS.iter().any(|m| text.contains(m))
}

fn legacy_notification_text(parts: &[Value]) -> bool {
    let text = text_of(parts);
    let text = text.trim_start();
    text.starts_with("<task-notification>") || text.starts_with("<subagent-notification>")
}

/// Node `getConversationMessageProjectionPolicy`.
pub fn policy(record: &Record) -> Policy {
    let info = &record.info;
    let parts = &record.parts;
    let semantics = info.get("semantics").filter(|s| s.is_object());
    // JSON null 也是 `summary !== undefined`：任何 summary 键都隐藏整条消息。
    if info["semantics"]["kind"] == "compact_summary" || info.get("summary").is_some() {
        return Policy::ProviderContextOnly;
    }
    if let Some(semantics) = semantics {
        if semantics["kind"] == "timeline_event" {
            return Policy::TimelineOnly;
        }
        if semantics["origin"] == "real_user"
            && info["synthetic"] != true
            && info["visibility"] != MODEL_ONLY
        {
            return Policy::RealUserInput;
        }
        if info["role"] == "assistant"
            && semantics["kind"] == "assistant_response"
            && semantics["uiVisibility"] == "visible"
            && semantics["transcriptVisibility"] == "visible"
        {
            return Policy::VisibleAssistant;
        }
        if semantics["providerVisibility"] == "visible" {
            return Policy::ProviderContextOnly;
        }
        if semantics["kind"] == "fork_notice" {
            return Policy::TimelineOnly;
        }
        if semantics["origin"] == "agent_runtime"
            || semantics["uiVisibility"] == "hidden"
            || semantics["transcriptVisibility"] == "hidden"
        {
            return Policy::HiddenSynthetic;
        }
    }
    if info["visibility"] == MODEL_ONLY || has_model_only_part(parts) {
        return Policy::ProviderContextOnly;
    }
    if timeline_only(record) {
        return Policy::TimelineOnly;
    }
    let source = source(record);
    if source == Some(FORK) {
        return Policy::TimelineOnly;
    }
    if source.is_some_and(|s| PROVIDER_CONTEXT_SOURCES.contains(&s)) {
        return Policy::ProviderContextOnly;
    }
    if legacy_reminder_text(parts) {
        return Policy::ProviderContextOnly;
    }
    let synthetic = info["synthetic"] == true || parts.iter().any(|p| p["synthetic"] == true);
    if synthetic && legacy_notification_text(parts) {
        return Policy::ProviderContextOnly;
    }
    if synthetic {
        return Policy::HiddenSynthetic;
    }
    if info["role"] == "assistant" {
        Policy::VisibleAssistant
    } else {
        Policy::RealUserInput
    }
}

/// Node `isConversationRealUserTurnStarter`.
pub fn real_user_starter(record: &Record) -> bool {
    record.info["role"] == "user" && policy(record) == Policy::RealUserInput
}

/// Node `getConversationModelOnlyTurnTriggerSource`.
pub fn model_only_trigger(record: &Record) -> Option<String> {
    if record.info["role"] != "user" || policy(record) != Policy::ProviderContextOnly {
        return None;
    }
    if let Some(source) = source(record).filter(|s| MODEL_ONLY_TRIGGER_SOURCES.contains(s)) {
        return Some(source.to_owned());
    }
    legacy_notification_text(&record.parts).then(|| "background_task".to_owned())
}
