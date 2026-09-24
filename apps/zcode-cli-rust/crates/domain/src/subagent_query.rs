//! `session/subagents` over the stored transcript (Node
//! `subagent-session-query.ts` and `listSessionSubagents`): Agent tool parts
//! of the parent's active branch, their child sessions and the live facts of
//! a resident runtime. Pure; spec rust-m11-node-storage §6.3.
use crate::node_history::{Branch, Record, select_branch};
use serde_json::{Map, Value, json};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use crate::subagent_outcome::{Outcome, last_outcome};
pub use crate::subagent_page::paginate;

const SUBAGENT_TOOLS: [&str; 3] = ["Agent", "Task", "subagent"];

/// Node `CANCELLATION_PATTERN` (`/abort|cancel|interrupt|stop/i`).
pub(crate) fn cancellation(text: &str) -> bool {
    let lower = text.to_lowercase();
    ["abort", "cancel", "interrupt", "stop"]
        .iter()
        .any(|word| lower.contains(word))
}

/// Node `nonEmptyString`.
pub(crate) fn non_empty(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Node `stringField`.
pub(crate) fn field(source: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| non_empty(&source[*key]))
}

pub(crate) fn object(value: &Value) -> Value {
    if value.is_object() {
        value.clone()
    } else {
        json!({})
    }
}

/// Node `agentIdFromLaunchAcknowledgement`.
fn launch_agent_id(output: &str) -> Option<String> {
    static REGEX: OnceLock<regress::Regex> = OnceLock::new();
    let regex = REGEX
        .get_or_init(|| regress::Regex::new(r"(?:^|\n)agentId:\s*([^\s(]+)").expect("valid regex"));
    let found = regex.find(output)?;
    non_empty(&Value::from(&output[found.group(1)?]))
}

/// Node `contentBlocksToText`.
fn blocks_text(value: &Value) -> Option<String> {
    let text = value
        .as_array()?
        .iter()
        .filter_map(|block| non_empty(&block["text"]))
        .collect::<Vec<_>>()
        .join("\n\n");
    (!text.is_empty()).then_some(text)
}

/// A live runtime's `SubagentSpawned`/`SubagentStopped` facts of one tool call
/// (Node `subagentEventRelations`).
#[derive(Clone, Debug, Default)]
pub struct Relation {
    pub agent_id: Option<String>,
    pub child_session_id: Option<String>,
    pub description: Option<String>,
    pub started_at: Option<u64>,
    pub stopped_at: Option<u64>,
    pub stopped_status: Option<&'static str>,
    pub subagent_type: Option<String>,
    pub summary: Option<String>,
}

/// A resident parent's background subagent task (Node `BackgroundTaskInfo`).
#[derive(Clone, Debug)]
pub struct BackgroundTask {
    pub task_id: String,
    pub tool_call_id: String,
    pub child_session_id: String,
    /// `running`, `completed`, `cancelled`, `failed`, `lost`.
    pub status: &'static str,
    pub started_at: Option<u64>,
    pub completed_at: Option<u64>,
}

/// What a resident runtime knows beyond the store.
#[derive(Clone, Debug, Default)]
pub struct Live {
    /// Relations by parent tool call id.
    pub relations: HashMap<String, Relation>,
    /// The parent is resident (Node `parentProjection`).
    pub parent: bool,
    pub background: Vec<BackgroundTask>,
    /// The parent's pending or running tool calls.
    pub active_calls: HashSet<String>,
    /// Resident children's projection status (`running`, `waiting`,
    /// `completed`, `error`).
    pub children: HashMap<String, &'static str>,
}

pub struct StoredChild {
    pub task_type: String,
    pub updated: u64,
    pub messages: Vec<Record>,
}

/// The stored parent (`time_updated`, `revert`, all messages) and its
/// subagent child sessions in candidate order.
pub struct StoredFacts {
    pub updated: u64,
    pub revert: Option<Value>,
    pub messages: Vec<Record>,
    pub children: Vec<(String, StoredChild)>,
}

struct Candidate<'a> {
    agent_id: String,
    child: String,
    background: bool,
    output: Value,
    part: &'a Value,
    subagent_type: String,
    summary: Option<String>,
    started_at: Option<u64>,
    stopped_at: Option<u64>,
    stopped_status: Option<&'static str>,
    title: String,
}

/// Node `candidateFromToolPart`.
fn candidate<'a>(part: &'a Value, relation: Option<&Relation>) -> Option<Candidate<'a>> {
    if !SUBAGENT_TOOLS.contains(&part["tool"].as_str()?) {
        return None;
    }
    let state = &part["state"];
    let completed = (state["status"] == "completed")
        .then(|| non_empty(&state["output"]))
        .flatten();
    // Node `parseJsonObject`：能解析但不是对象时为空对象。
    let output = completed
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .map(|v| object(&v))
        .unwrap_or(Value::Null);
    let input = object(&state["input"]);
    let mut metadata = object(&part["metadata"]);
    if let Some(extra) = state.get("metadata").and_then(Value::as_object) {
        for (key, value) in extra {
            metadata[key] = value.clone();
        }
    }
    let agent_id = field(&output, &["agentId"])
        .or_else(|| completed.as_deref().and_then(launch_agent_id))
        .or_else(|| field(&metadata, &["agentId"]))
        .or_else(|| relation.and_then(|r| r.agent_id.clone()));
    let child = field(&output, &["childSessionId"])
        .or_else(|| field(&metadata, &["childSessionId"]))
        .or_else(|| relation.and_then(|r| r.child_session_id.clone()))
        .or_else(|| agent_id.as_ref().map(|id| format!("sess_subagent_{id}")))?;
    let title = field(&output, &["description"])
        .or_else(|| field(&input, &["description"]))
        .or_else(|| field(&metadata, &["description"]))
        .or_else(|| relation.and_then(|r| r.description.clone()))
        .or_else(|| field(&input, &["prompt"]))
        .unwrap_or_else(|| "Subagent".into());
    let subagent_type = field(&output, &["agentType"])
        .or_else(|| field(&metadata, &["agentType"]))
        .or_else(|| relation.and_then(|r| r.subagent_type.clone()))
        .or_else(|| field(&input, &["subagent_type", "agent", "agentType"]))
        .unwrap_or_else(|| "subagent".into());
    let summary = blocks_text(&output["content"])
        .or_else(|| field(&output, &["result", "summary"]))
        .or_else(|| {
            (state["status"] == "error")
                .then(|| non_empty(&state["error"]))
                .flatten()
        })
        .or_else(|| relation.and_then(|r| r.summary.clone()));
    Some(Candidate {
        agent_id: agent_id.unwrap_or_else(|| part["callID"].as_str().unwrap_or("").to_owned()),
        child,
        background: input["run_in_background"] == true,
        output,
        part,
        subagent_type,
        summary,
        started_at: relation.and_then(|r| r.started_at),
        stopped_at: relation.and_then(|r| r.stopped_at),
        stopped_status: relation.and_then(|r| r.stopped_status),
        title,
    })
}

/// Node `collectCandidates` over the active branch (later parts win, Map order).
fn candidates<'a>(
    messages: &'a [Record],
    revert: Option<&Value>,
    live: &Live,
) -> Vec<Candidate<'a>> {
    let branch = Branch::from_revert(revert);
    let active = select_branch(messages.iter().map(Cow::Borrowed).collect(), &branch);
    let mut out: Vec<Candidate<'a>> = vec![];
    for message in active {
        let Cow::Borrowed(message) = message else {
            continue;
        };
        for part in message.parts.iter().filter(|p| p["type"] == "tool") {
            let call = part["callID"].as_str().unwrap_or("");
            let Some(found) = candidate(part, live.relations.get(call)) else {
                continue;
            };
            match out.iter().position(|c| c.child == found.child) {
                Some(index) => out[index] = found,
                None => out.push(found),
            }
        }
    }
    out
}

/// Node `collectSubagentChildSessionIds`.
pub fn child_session_ids(messages: &[Record], revert: Option<&Value>, live: &Live) -> Vec<String> {
    candidates(messages, revert, live)
        .into_iter()
        .map(|c| c.child)
        .collect()
}

fn background_of<'a>(live: &'a Live, c: &Candidate) -> Option<&'a BackgroundTask> {
    let call = c.part["callID"].as_str().unwrap_or("");
    live.background.iter().find(|t| {
        t.child_session_id == c.child || t.tool_call_id == call || t.task_id == c.agent_id
    })
}

/// Node `runningStatus`.
fn running_status(
    c: &Candidate,
    background: Option<&BackgroundTask>,
    outcome: &Outcome,
    child: Option<&'static str>,
    live: &Live,
) -> Option<&'static str> {
    if background.is_some_and(|b| b.status == "running") {
        return Some("running");
    }
    if let Some(status @ ("waiting" | "running")) = child {
        return Some(status);
    }
    if c.background
        && background.is_none()
        && child.is_none()
        && c.stopped_status.is_none()
        && outcome.status.is_none()
    {
        return Some("running");
    }
    let call = c.part["callID"].as_str().unwrap_or("");
    let open = matches!(
        c.part["state"]["status"].as_str(),
        Some("pending" | "running")
    );
    (live.active_calls.contains(call) || (live.parent && open)).then_some("running")
}

/// Node `endedStatus`.
fn ended_status(
    c: &Candidate,
    background: Option<&BackgroundTask>,
    outcome: &Outcome,
    child: Option<&'static str>,
) -> &'static str {
    match background.map(|b| b.status) {
        Some("completed") => return "success",
        Some("cancelled") => return "cancelled",
        Some("failed") => return "failed",
        Some("lost") => return "lost",
        _ => {}
    }
    match child {
        Some("error") => return "failed",
        Some("completed") => return "success",
        _ => {}
    }
    let state = &c.part["state"];
    if state["status"] == "error" {
        return if cancellation(state["error"].as_str().unwrap_or("")) {
            "cancelled"
        } else {
            "failed"
        };
    }
    if let Some(status) = c.stopped_status {
        return status;
    }
    match field(&c.output, &["status"]).as_deref() {
        Some("cancelled" | "stopped") => return "cancelled",
        Some("failed" | "error") => return "failed",
        Some("async_launched") => return outcome.status.unwrap_or("lost"),
        _ => {}
    }
    if state["status"] == "completed" {
        return "success";
    }
    outcome.status.unwrap_or("lost")
}

/// Node `projectSessionSubagents`: `(running, ended)`, newest first.
pub fn project(
    messages: &[Record],
    revert: Option<&Value>,
    children: &[(String, StoredChild)],
    live: &Live,
) -> (Vec<Value>, Vec<Value>) {
    let (mut running, mut ended) = (vec![], vec![]);
    for c in candidates(messages, revert, live) {
        let Some((_, stored)) = children
            .iter()
            .find(|(id, s)| *id == c.child && s.task_type == "subagent_child")
        else {
            continue;
        };
        let child = live.children.get(&c.child).copied();
        let background = background_of(live, &c);
        let outcome = last_outcome(Some(&stored.messages));
        let state = &c.part["state"];
        let started = background
            .and_then(|b| b.started_at)
            .or(c.started_at)
            .or_else(|| state["time"]["start"].as_u64());
        let mut item = Map::new();
        item.insert("childSessionId".into(), c.child.clone().into());
        item.insert("agentId".into(), c.agent_id.clone().into());
        item.insert("toolCallId".into(), c.part["callID"].clone());
        item.insert("subagentType".into(), c.subagent_type.clone().into());
        item.insert("title".into(), c.title.clone().into());
        if let Some(started) = started {
            item.insert("startedAt".into(), started.into());
        }
        if let Some(status) = running_status(&c, background, &outcome, child, live) {
            item.insert("status".into(), status.into());
            running.push(Value::Object(item));
            continue;
        }
        item.insert(
            "status".into(),
            ended_status(&c, background, &outcome, child).into(),
        );
        if let Some(summary) = c.summary.clone().or(outcome.summary.clone()) {
            item.insert("summary".into(), summary.into());
        }
        let ended_at = background
            .and_then(|b| b.completed_at)
            .or(c.stopped_at)
            .or_else(|| state["time"]["end"].as_u64())
            .or(outcome.ended_at)
            .unwrap_or(stored.updated);
        item.insert("endedAt".into(), ended_at.into());
        ended.push(Value::Object(item));
    }
    let order = |key: &'static str| {
        move |a: &Value, b: &Value| {
            let at = |v: &Value| v[key].as_u64().unwrap_or(0);
            at(b).cmp(&at(a)).then_with(|| {
                b["childSessionId"]
                    .as_str()
                    .cmp(&a["childSessionId"].as_str())
            })
        }
    };
    running.sort_by(order("startedAt"));
    ended.sort_by(order("endedAt"));
    (running, ended)
}

#[cfg(test)]
#[path = "subagent_query_tests.rs"]
mod tests;
