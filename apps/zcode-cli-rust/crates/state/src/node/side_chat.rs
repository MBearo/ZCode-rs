//! Node's selection side chat (`createSelectionSideConversation` and the
//! `selection_side_chat` kind of `commitAtomicConversationFork`): the parent's
//! active transcript up to the running turn's input, copied under child-local
//! identities as model-only history, then a boundary reminder. No goal or
//! verifier state is copied. The caller owns the transaction.
use super::fork::{COMMAND_FACT_ENTRY, identities, selection};
use super::fork_bundle::Bundle;
use super::fork_clone::{self as clone, local};
use super::messages::{CopyFrom, save_message, save_part};
use super::{cold, entries, sessions};
use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};
use std::borrow::Cow;
use zcode_cli_domain::node_history::{Branch, Record, select_branch};
use zcode_cli_domain::node_ids;

/// Node `SELECTION_SIDE_CHAT_BOUNDARY`.
const BOUNDARY: &str = "The preceding conversation was inherited from the parent task for reference only. Do not continue the parent's active work automatically; answer only new questions sent in this side chat. Modify the workspace only when the user explicitly asks you to do so in this side chat.";

/// Node `selectionSideChatHistoryMessages`: while a turn runs, only its
/// committed real-user input is kept.
fn history<'a>(active: Vec<Cow<'a, Record>>, turn: Option<&str>) -> Vec<Cow<'a, Record>> {
    let Some(turn) = turn else {
        return active;
    };
    let in_turn = |m: &Record| m.info["anchor"]["turnId"] == turn;
    if let Some(user) = active.iter().position(|m| {
        m.info["role"] == "user" && in_turn(m) && m.info["anchor"]["origin"] == "realUser"
    }) {
        return active[..=user].to_vec();
    }
    match active.iter().position(|m| in_turn(m)) {
        Some(start) => active[..start].to_vec(),
        None => active,
    }
}

/// Node `withoutSelectionSideChatGoalBoundary`.
fn without_goal_boundary(record: &Record) -> Record {
    let mut record = record.clone();
    if let Some(anchor) = record.info["anchor"].as_object_mut() {
        anchor.shift_remove("goalBoundary");
    }
    record
}

/// The copied message as model-only history (the side chat starts empty).
fn hidden(mut info: Value) -> Value {
    let semantics = info["semantics"].clone();
    let mut next = Map::new();
    let text = |key: &str, default: &str| {
        semantics[key]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(default)
            .to_owned()
    };
    next.insert("origin".into(), text("origin", "migration").into());
    next.insert("kind".into(), text("kind", "system_reminder").into());
    if let Some(source) = semantics["source"].as_str().filter(|s| !s.is_empty()) {
        next.insert("source".into(), source.into());
    }
    next.insert("uiVisibility".into(), "hidden".into());
    next.insert("providerVisibility".into(), "visible".into());
    next.insert("transcriptVisibility".into(), "hidden".into());
    info["visibility"] = "model-only".into();
    info["semantics"] = Value::Object(next);
    info
}

/// Node `buildSelectionSideChatBoundary`.
fn boundary(child: &str, selection: Option<&Value>, now: u64) -> (Value, Value) {
    let message = node_ids::message_id(now, &crate::id());
    let turn = node_ids::turn_id(&crate::id());
    let mut info = json!({"id": message, "sessionID": child, "role": "user",
        "time": {"created": now}, "agent": "zcode-agent"});
    if let Some(selection) = selection {
        info["modelSelection"] = selection.clone();
    }
    info["synthetic"] = true.into();
    info["source"] = "selection_side_chat".into();
    info["visibility"] = "model-only".into();
    info["semantics"] = json!({"origin": "system", "kind": "system_reminder",
        "source": "selection_side_chat", "uiVisibility": "hidden", "providerVisibility": "visible",
        "transcriptVisibility": "hidden"});
    info["anchor"] = json!({"turnId": turn, "productTurnId": message,
        "orderedMessageIds": [message], "boundaryMessageId": message, "origin": "synthetic"});
    let part = json!({"id": node_ids::part_id(now, &crate::id()), "sessionID": child,
        "messageID": message, "type": "text", "text": BOUNDARY, "synthetic": true,
        "time": {"start": now, "end": now},
        "metadata": {"runtimeMessage": {"source": "selection_side_chat"},
            "source": "selection_side_chat", "visibility": "model-only"}});
    (info, part)
}

/// Commits the selection side chat of `request.parent` as `child`.
pub fn side_chat(
    conn: &rusqlite::Connection,
    child: &str,
    request: &Value,
    now: i64,
) -> Result<()> {
    let parent_id = request["parent"].as_str().context("side chat parent")?;
    let command = request["command"].as_str().context("side chat command")?;
    let fact_id = format!("v4_command_fact:child:{parent_id}:{command}");
    if entries::list(conn, parent_id, Some(COMMAND_FACT_ENTRY))?
        .iter()
        .any(|e| e.id == fact_id)
    {
        bail!("Side chat command already committed: {fact_id}");
    }
    let parent = sessions::get(conn, parent_id)?.context("Side chat parent missing")?;
    let all = cold::records(conn, parent_id)?;
    let branch = Branch::from_revert(parent.revert.as_ref());
    let active = select_branch(all.iter().map(Cow::Borrowed).collect(), &branch);
    let messages: Vec<Cow<Record>> = history(active, request["activeTurn"].as_str())
        .iter()
        .map(|m| Cow::Owned(without_goal_boundary(m)))
        .collect();
    let target = messages.last().map_or_else(
        || node_ids::message_id(now as u64, &crate::id()),
        |m| m.id().to_owned(),
    );
    let ids = identities(child, &messages, &[], &[], now as u64);
    // Node 显式选择（首条输入推荐或父会话当前选择）优先，否则按历史解析。
    let selection = match request.get("selection").filter(|s| s.is_object()) {
        Some(explicit) => Some(explicit.clone()),
        None => selection(&messages, &Value::Null),
    };
    let execution = json!({"mode": request["execution"]["mode"],
        "planEnabled": request["execution"]["planEnabled"] == true});
    let target = Value::from(target);
    let bundle = Bundle {
        kind: "selection_side_chat",
        parent: &parent,
        ids: &ids,
        selection: selection.as_ref(),
        execution: &execution,
        command,
        target: &target,
        now,
    };
    bundle.create_child(conn)?;
    for m in &messages {
        let next = local(&ids.messages, m.id(), "message")?;
        let source = |id| {
            Some(CopyFrom {
                session_id: parent_id,
                id,
            })
        };
        let info = hidden(clone::message(&m.info, &ids, &next)?);
        save_message(conn, &info, source(m.id()), now)?;
        for part in &m.parts {
            let copy = clone::part(part, &ids, &next)?;
            save_part(conn, &copy, source(part["id"].as_str().unwrap_or("")), now)?;
        }
    }
    let (info, part) = boundary(child, selection.as_ref(), now as u64);
    save_message(conn, &info, None, now)?;
    save_part(conn, &part, None, now)?;
    for entry in bundle.entries() {
        entries::save(conn, &entry)?;
    }
    entries::save(
        conn,
        &bundle.command_fact(&fact_id, request["revision"].as_u64().unwrap_or(0)),
    )?;
    Ok(())
}
