//! Applying a session's journal writes with Node's repository SQL (spec
//! rust-m11-node-storage §5.1). The caller owns the transaction.
use super::entries::{self, Entry};
use super::inputs;
use super::messages::{self, remove_message, save_message, save_part};
use super::sessions::{self, Create, Update};
use super::targets;
use super::todos::{self, Todo};
use anyhow::{Context, Result};
use rusqlite::Connection;
use serde_json::{Value, json};
use zcode_cli_domain::node_journal::{Op, Write};

const VERIFICATION_ENTRY: &str = "target_completion_verification";

fn text(value: &Value, key: &str) -> Option<String> {
    value[key].as_str().map(str::to_owned)
}

fn create(v: &Value) -> Create {
    Create {
        id: text(v, "id").unwrap_or_default(),
        project_id: text(v, "projectID").unwrap_or_default(),
        workspace_id: text(v, "workspaceID"),
        parent_id: text(v, "parentID"),
        trace_id: text(v, "traceID"),
        task_type: text(v, "taskType"),
        slug: text(v, "slug").unwrap_or_default(),
        directory: text(v, "directory").unwrap_or_default(),
        path: text(v, "path"),
        title: text(v, "title").unwrap_or_default(),
        title_source: text(v, "titleSource"),
        version: text(v, "version").unwrap_or_default(),
        permission: v.get("permission").cloned(),
        ..Create::default()
    }
}

fn update(v: &Value) -> Update {
    Update {
        id: text(v, "id").unwrap_or_default(),
        title: text(v, "title"),
        title_source: text(v, "titleSource"),
        ..Update::default()
    }
}

fn entry(v: &Value) -> Entry {
    Entry {
        id: text(v, "id").unwrap_or_default(),
        session_id: text(v, "sessionID").unwrap_or_default(),
        kind: text(v, "type").unwrap_or_default(),
        time_created: v["time"]["created"].as_i64().unwrap_or(0),
        time_updated: v["time"]["updated"].as_i64().unwrap_or(0),
        data: v["data"].clone(),
        touch_session: v["touchSession"] != false,
    }
}

fn todo(v: &Value) -> Todo {
    Todo {
        content: text(v, "content").unwrap_or_default(),
        status: text(v, "status").unwrap_or_default(),
        priority: text(v, "priority").unwrap_or_default(),
    }
}

/// The session's queued writes, in order.
pub fn apply(conn: &Connection, session: &str, writes: &[Write]) -> Result<()> {
    for write in writes {
        let now = write.at as i64;
        match &write.op {
            Op::CreateSession(v) => {
                sessions::create(conn, &create(v), now)?;
            }
            Op::UpdateSession(v) => {
                sessions::update(conn, &update(v), now)?;
            }
            Op::SaveInput(v) => inputs::save(
                conn,
                v["id"].as_str().context("input id")?,
                session,
                v["kind"].as_str().unwrap_or("sendText"),
                v["delivery"].as_str().unwrap_or("queue"),
                &v["payload"],
                now,
            )?,
            Op::PromoteInput { id, message, parts } => {
                inputs::promote(conn, id, session, message, parts, now)?
            }
            Op::Message(info) => save_message(conn, info, None, now)?,
            Op::Part(part) => save_part(conn, part, None, now)?,
            Op::RemoveMessage(id) => remove_message(conn, session, id)?,
            Op::Entry(v) => entries::save(conn, &entry(v))?,
            Op::Todos(list) => todos::update(
                conn,
                session,
                &list.iter().map(todo).collect::<Vec<_>>(),
                now,
            )?,
            Op::StableBoundary {
                boundary,
                start,
                rounds,
                turn,
            } => stable_boundary(conn, session, (boundary, start), *rounds, turn, now)?,
        }
    }
    Ok(())
}

/// Node `stableGoalSnapshot`: the goal without its transient run.
fn stable_goal(target: &targets::Target) -> Value {
    let mut goal = target.to_node();
    for key in [
        "activeInputId",
        "activeRunStartedAtMs",
        "activeRunLastSeenAtMs",
    ] {
        goal[key] = Value::Null;
    }
    goal
}

/// Node `persistStableForkCompletionBoundary`: the final assistant's anchor
/// fixes the turn's exact message segment and goal boundary.
fn stable_boundary(
    conn: &Connection,
    session: &str,
    (boundary, start): (&str, &str),
    rounds: u64,
    turn: &str,
    now: i64,
) -> Result<()> {
    let stored = messages::messages(conn, session)?;
    let completed_assistant = |m: &messages::WithParts| {
        m.info["role"] == "assistant"
            && m.info.get("error").is_none_or(Value::is_null)
            && m.info["time"].get("completed").is_some()
    };
    let Some(end) = stored
        .iter()
        .rposition(|m| m.info["id"] == boundary && completed_assistant(m))
    else {
        return Ok(());
    };
    let Some(begin) = stored[..=end].iter().rposition(|m| m.info["id"] == start) else {
        return Ok(());
    };
    let ordered: Vec<Value> = stored[begin..=end]
        .iter()
        .map(|m| m.info["id"].clone())
        .collect();
    let prefix: Vec<&Value> = stored[..=end].iter().map(|m| &m.info["id"]).collect();
    let goal = match targets::read(conn, session)? {
        None => json!({"kind": "none"}),
        Some(target) => {
            let ids: Vec<Value> = entries::list(conn, session, Some(VERIFICATION_ENTRY))?
                .into_iter()
                .filter(|e| e.data["payload"]["targetId"] == target.target_id.as_str())
                .filter(|e| {
                    let anchor = &e.data["payload"]["anchorAssistantMessageId"];
                    anchor.as_str().is_none_or(str::is_empty) || prefix.contains(&anchor)
                })
                .map(|e| e.id.into())
                .collect();
            json!({"kind": "snapshot", "target": stable_goal(&target), "verificationEntryIds": ids})
        }
    };
    let mut info = stored[end].info.clone();
    let mut anchor = info["anchor"].as_object().cloned().unwrap_or_default();
    if !turn.is_empty() {
        anchor.insert("turnId".into(), turn.into());
    }
    anchor.insert("historyRoundCount".into(), rounds.into());
    anchor.insert("orderedMessageIds".into(), ordered.into());
    anchor.insert("boundaryMessageId".into(), boundary.into());
    anchor.insert("goalBoundary".into(), goal);
    info["anchor"] = Value::Object(anchor);
    save_message(conn, &info, None, now)
}
