//! Subagent lifecycles the cold synthesis rebuilds from Agent tool parts and
//! legacy subtask parts (Node `subagentInfoFromToolPart`,
//! `synthesizeSubagentLifecycle`, `synthesizeSubtaskPart`).
use super::facts::{js_string, text};
use super::synth::Synth;
use super::synth_parts::put;
use serde_json::{Value, json};
use std::sync::OnceLock;

const SUBAGENT_TOOLS: [&str; 3] = ["Agent", "Task", "subagent"];

/// Node `ParsedSubagentOutput`; members are JS values, absent when undefined.
#[derive(Default)]
pub(super) struct SubagentInfo {
    agent_id: Option<Value>,
    agent_type: Option<Value>,
    child_session_id: Option<Value>,
    description: Option<Value>,
    parent_tool_call_id: Option<Value>,
    prompt: Option<Value>,
    summary_text: Option<Value>,
}

fn field(source: &Value, key: &str) -> Option<Value> {
    text(&source[key]).map(Value::from)
}

fn agent_id_regex() -> &'static regress::Regex {
    static REGEX: OnceLock<regress::Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        regress::Regex::with_flags(r"(?:^|\r?\n)agentId:\s*([^\s(]+)", "u").expect("valid regex")
    })
}

/// Node `agentIdFromToolOutput`.
fn agent_id_from_output(output: &str) -> Option<String> {
    let found = agent_id_regex().find(output)?;
    found.group(1).map(|range| output[range].to_owned())
}

/// Node `contentBlocksToText`.
fn content_blocks(value: &Value) -> Option<Value> {
    let chunks: Vec<&str> = value
        .as_array()?
        .iter()
        .filter_map(|block| block.as_object().and_then(|b| b.get("text")?.as_str()))
        .filter(|t| !t.is_empty())
        .collect();
    (!chunks.is_empty()).then(|| chunks.join("\n\n").into())
}

/// Node `subagentInfoFromToolPart`.
pub(super) fn subagent_info(part: &Value) -> Option<SubagentInfo> {
    if !SUBAGENT_TOOLS.contains(&part["tool"].as_str()?) {
        return None;
    }
    let state = &part["state"];
    let empty = json!({});
    let input = if state["input"].is_object() || state["input"].is_array() {
        &state["input"]
    } else {
        &empty
    };
    let completed = state["status"] == "completed";
    let output = completed
        .then(|| state["output"].as_str())
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .filter(Value::is_object)
        .unwrap_or(Value::Null);
    let metadata = if part["metadata"].is_object() || part["metadata"].is_array() {
        &part["metadata"]
    } else {
        &empty
    };
    let explicit = field(&output, "agentId")
        .or_else(|| field(metadata, "agentId"))
        .or_else(|| {
            let raw = completed.then(|| state["output"].as_str()).flatten()?;
            agent_id_from_output(raw).map(Value::from)
        });
    let agent_id = explicit.clone().unwrap_or_else(|| part["callID"].clone());
    Some(SubagentInfo {
        agent_type: field(&output, "agentType")
            .or_else(|| field(metadata, "agentType"))
            .or_else(|| field(input, "agent"))
            .or_else(|| field(input, "agentType"))
            .or_else(|| Some("subagent".into())),
        child_session_id: field(&output, "childSessionId")
            .or_else(|| field(metadata, "childSessionId"))
            .or_else(|| {
                explicit
                    .as_ref()
                    .map(|_| format!("sess_subagent_{}", js_string(&agent_id)).into())
            }),
        description: field(&output, "description")
            .or_else(|| field(input, "description"))
            .or_else(|| field(metadata, "description")),
        parent_tool_call_id: part.get("callID").cloned(),
        prompt: field(&output, "prompt").or_else(|| field(input, "prompt")),
        summary_text: content_blocks(&output["content"])
            .or_else(|| field(&output, "result"))
            .or_else(|| field(&output, "summary"))
            .or_else(|| field(input, "description"))
            .or_else(|| field(input, "prompt")),
        agent_id: Some(agent_id),
    })
}

/// Node `synthesizeSubagentLifecycle`.
pub(super) fn subagent_lifecycle(s: &mut Synth, info: SubagentInfo, status: &str, turn: &str) {
    let agent_id = info
        .agent_id
        .clone()
        .unwrap_or_else(|| format!("subagent-{turn}").into());
    let agent_type = info.agent_type.clone().unwrap_or_else(|| "subagent".into());
    let description = info
        .description
        .clone()
        .or_else(|| info.summary_text.clone())
        .or_else(|| info.prompt.clone())
        .unwrap_or_else(|| agent_id.clone());
    let mut spawned = json!({"agentId": agent_id, "agentType": agent_type});
    put(
        &mut spawned,
        "childSessionId",
        info.child_session_id.as_ref(),
    );
    spawned["description"] = description;
    put(
        &mut spawned,
        "parentToolCallId",
        info.parent_tool_call_id.as_ref(),
    );
    put(&mut spawned, "prompt", info.prompt.as_ref());
    spawned["status"] = "running".into();
    s.push("subagent_spawned", spawned, Some(turn), None);
    let mut stopped = json!({"agentId": agent_id, "agentType": agent_type});
    put(
        &mut stopped,
        "childSessionId",
        info.child_session_id.as_ref(),
    );
    put(&mut stopped, "description", info.description.as_ref());
    put(
        &mut stopped,
        "parentToolCallId",
        info.parent_tool_call_id.as_ref(),
    );
    put(&mut stopped, "prompt", info.prompt.as_ref());
    put(&mut stopped, "summaryText", info.summary_text.as_ref());
    stopped["status"] = status.into();
    s.push("subagent_stopped", stopped, Some(turn), None);
}

pub fn subtask_part(s: &mut Synth, part: &Value, turn: &str) {
    let non_null = |key: &str| part.get(key).filter(|v| !v.is_null()).cloned();
    let info = SubagentInfo {
        agent_id: Some(js_string(&part["id"]).into()),
        agent_type: non_null("agent"),
        description: non_null("description"),
        prompt: non_null("prompt"),
        summary_text: non_null("description"),
        ..SubagentInfo::default()
    };
    subagent_lifecycle(s, info, "completed", turn);
}
