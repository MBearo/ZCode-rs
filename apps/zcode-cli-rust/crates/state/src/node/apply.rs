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
use std::borrow::Cow;
use zcode_cli_domain::node_history::{self, Branch};
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
        time_created: v["time"]["created"].as_i64(),
        time_updated: v["time"]["updated"].as_i64(),
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

pub(super) fn entry(v: &Value) -> Entry {
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

fn patch(v: &Value) -> inputs::Patch {
    inputs::Patch {
        id: text(v, "id").unwrap_or_default(),
        delivery: text(v, "delivery"),
        intent: v.get("intent").filter(|i| !i.is_null()).cloned(),
        text: text(v, "text"),
        queue_position: v["queuePosition"].as_i64(),
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
            Op::UpdateInputs(updates) => {
                let patches: Vec<inputs::Patch> = updates.iter().map(patch).collect();
                inputs::update(conn, session, &patches, now)?
            }
            Op::MarkInputPromoted { id, message } => {
                inputs::mark_promoted(conn, id, session, message, now)?
            }
            Op::SettleInput { id, status, reason } => {
                inputs::settle(conn, id, session, status, reason.as_deref(), now)?
            }
            Op::Rewind { target, anchor } => rewind(conn, session, (target, anchor), now)?,
            Op::Fork(request) => super::fork::fork(conn, session, request, now)?,
            Op::CompactSummary(done) => super::compact::summary(conn, session, done, now)?,
            Op::Todos(list) => todos::update(
                conn,
                session,
                &list.iter().map(todo).collect::<Vec<_>>(),
                now,
            )?,
            Op::Target(goal) => super::target_row::put(conn, session, goal.as_ref(), now)?,
            Op::SharedTransition {
                context,
                from,
                to,
                source,
            } => {
                let states = (from.as_slice(), to.as_str());
                super::shared::transition(conn, session, context, states, source.as_deref(), now)?;
            }
            Op::FullAccess {
                queue,
                execution,
                receipt,
            } => {
                let pair = (&entry(execution), &entry(receipt));
                super::full_access::commit(conn, session, queue, pair, now)?
            }
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

/// Node `applyConversationRewindPlan`: the active branch before `target` is
/// kept, everything stored so far is cut, the branch generation advances.
fn rewind(
    conn: &Connection,
    session: &str,
    (target, anchor): (&str, &str),
    now: i64,
) -> Result<()> {
    let Some(row) = sessions::get(conn, session)? else {
        return Ok(());
    };
    let all = super::cold::records(conn, session)?;
    let branch = Branch::from_revert(row.revert.as_ref());
    let active = node_history::select_branch(all.iter().map(Cow::Borrowed).collect(), &branch);
    let index = active
        .iter()
        .position(|m| m.id() == target)
        .context("Rewind target is not in the active branch")?;
    let kept: Vec<Value> = active[..index].iter().map(|m| m.id().into()).collect();
    let generation = row
        .revert
        .as_ref()
        .and_then(|r| r["branchGeneration"].as_i64())
        .unwrap_or(0)
        + 1;
    let revert = json!({
        "keptMessageIDs": kept,
        "branchCutAfterMessageID": all.last().map(|m| m.id()),
        "branchGeneration": generation,
        "messageID": kept.last().cloned().unwrap_or_else(|| anchor.into()),
        "kind": "conversation_rewind",
        "scope": "conversation",
        "targetMessageID": anchor,
    });
    sessions::set_revert(conn, session, revert, None, now)?;
    Ok(())
}
