//! Node's stable conversation fork (`resolveStableForkTargetFromTranscript`,
//! `forkStableConversationAtMessage`, `commitAtomicConversationFork`,
//! `commitForkBundle`): the child session, its copied transcript under
//! child-local identities, the fork notice, the child's model selection and
//! execution state, copied goal verifier entries and goal, and the parent's
//! child command fact. The caller owns the transaction.
use super::fork_clone::{self as clone, Identities, local};
use super::messages::{CopyFrom, save_message, save_part};
use super::{cold, entries, sessions, targets};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::borrow::Cow;
use zcode_cli_domain::node_history::{Branch, Record, select_branch};
use zcode_cli_domain::node_ids;

const VERIFICATION_ENTRY: &str = "target_completion_verification";
const COMMAND_FACT_ENTRY: &str = "v4/command_fact";

fn uuid() -> String {
    crate::id()
}

/// The persisted fork target of the boundary (Node `persistedTarget`); the
/// anchor is completed on the parent like Node's `persistResolvedAnchor`.
fn target(
    conn: &rusqlite::Connection,
    active: &[Cow<Record>],
    boundary: &str,
    now: i64,
) -> Result<(Value, Value)> {
    let record = active
        .iter()
        .find(|m| m.id() == boundary)
        .context("guard.forkTargetAmbiguous")?;
    let anchor = &record.info["anchor"];
    let ordered: Vec<&str> = anchor["orderedMessageIds"]
        .as_array()
        .map(|ids| ids.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let positions: Vec<Option<usize>> = ordered
        .iter()
        .map(|id| active.iter().position(|m| m.id() == *id))
        .collect();
    let increasing = positions
        .windows(2)
        .all(|w| matches!(w, [Some(a), Some(b)] if a < b));
    ensure!(
        !ordered.is_empty()
            && ordered.last() == Some(&boundary)
            && anchor["boundaryMessageId"] == boundary
            && positions.iter().all(Option::is_some)
            && increasing,
        "guard.forkTargetAmbiguous"
    );
    let turn = anchor["turnId"].as_str().unwrap_or("");
    let product = anchor["productTurnId"].as_str().unwrap_or(turn);
    let target = json!({"productTurnId": product, "transcriptTurnId": turn,
        "orderedMessageIds": ordered, "boundaryMessageId": boundary});
    let goal = match anchor.get("goalBoundary").filter(|g| !g.is_null()) {
        Some(goal) => goal.clone(),
        None => {
            // Node legacyGoalBoundary：父会话仍有 goal 或验证记录时无法还原分叉点的 goal。
            let session = record.info["sessionID"].as_str().unwrap_or("");
            let verified = !entries::list(conn, session, Some(VERIFICATION_ENTRY))?.is_empty();
            ensure!(
                targets::read(conn, session)?.is_none() && !verified,
                "guard.forkTargetAmbiguous"
            );
            json!({"kind": "none"})
        }
    };
    if anchor.get("goalBoundary").is_none_or(Value::is_null)
        || anchor.get("productTurnId").is_none()
    {
        let mut info = record.info.clone();
        let mut next = anchor.as_object().cloned().unwrap_or_default();
        next.insert("productTurnId".into(), product.into());
        next.insert(
            "orderedMessageIds".into(),
            target["orderedMessageIds"].clone(),
        );
        next.insert("boundaryMessageId".into(), boundary.into());
        next.insert("goalBoundary".into(), goal.clone());
        info["anchor"] = Value::Object(next);
        save_message(conn, &info, None, now)?;
    }
    Ok((target, goal))
}

/// Node `stableForkHistoryMessages`: the active prefix and the target segment.
fn history<'a>(active: &[Cow<'a, Record>], target: &Value) -> Result<Vec<Cow<'a, Record>>> {
    let ordered = target["orderedMessageIds"]
        .as_array()
        .context("fork target")?;
    let first = ordered[0].as_str().unwrap_or("");
    let start = active
        .iter()
        .position(|m| m.id() == first)
        .context("guard.forkTargetAmbiguous")?;
    let segment = active
        .get(start..start + ordered.len())
        .context("guard.forkTargetAmbiguous")?;
    ensure!(
        segment
            .iter()
            .zip(ordered)
            .all(|(m, id)| m.id() == id.as_str().unwrap_or("")),
        "Stable fork target is not a contiguous active transcript segment"
    );
    let boundary = &segment[segment.len() - 1].info;
    ensure!(
        boundary["role"] == "assistant" && boundary.get("error").is_none_or(Value::is_null),
        "Stable fork boundary is not a completed assistant message"
    );
    Ok(active[..start + ordered.len()].to_vec())
}

/// Node `resolveForkModelSelection` (`{modelId, providerId, options?}` order
/// is normalised by `cloneModelSelection` where it is written).
fn selection(messages: &[Cow<Record>], runtime: &Value) -> Option<Value> {
    let historical = messages.iter().rev().find_map(|m| {
        let info = &m.info;
        if info["role"] == "user" {
            return info
                .get("modelSelection")
                .filter(|s| s.is_object())
                .cloned();
        }
        let (model, provider) = (info["modelId"].as_str()?, info["providerId"].as_str()?);
        let mut out = json!({"providerId": provider, "modelId": model});
        if let Some(level) = info["reasoningLevel"].as_str().filter(|l| !l.is_empty()) {
            out["options"] = json!({"reasoningLevel": level});
        }
        Some(out)
    });
    let runtime = runtime.is_object().then_some(runtime);
    let identity = historical.as_ref().or(runtime)?;
    let level = historical
        .as_ref()
        .and_then(|h| h["options"].get("reasoningLevel").cloned())
        .or_else(|| runtime.and_then(|r| r["options"].get("reasoningLevel").cloned()));
    let mut out = json!({"providerId": identity["providerId"], "modelId": identity["modelId"]});
    if let Some(level) = level {
        out["options"] = json!({"reasoningLevel": level});
    }
    Some(out)
}

/// Node `createForkIdentityMap` over the copied transcript and verifier entries.
fn identities(
    child: &str,
    messages: &[Cow<Record>],
    goals: &[&Value],
    verifiers: &[entries::Entry],
    now: u64,
) -> Identities {
    let mut ids = Identities {
        child: child.into(),
        ..Identities::default()
    };
    for m in messages {
        ids.messages
            .insert(m.id().into(), node_ids::message_id(now, &uuid()));
        for part in &m.parts {
            ids.parts.insert(
                part["id"].as_str().unwrap_or("").into(),
                node_ids::part_id(now, &uuid()),
            );
            if part["type"] == "tool" {
                let call = part["callID"].as_str().unwrap_or("").to_owned();
                ids.tool_calls.insert(call, format!("tool_{}", uuid()));
                for attachment in part["state"]["attachments"]
                    .as_array()
                    .into_iter()
                    .flatten()
                {
                    let id = attachment["id"].as_str().unwrap_or("").to_owned();
                    ids.parts.insert(id, node_ids::part_id(now, &uuid()));
                }
            }
        }
    }
    let turn = |ids: &mut Identities, id: &Value| {
        if let Some(id) = id.as_str().filter(|s| !s.is_empty()) {
            ids.turns
                .entry(id.into())
                .or_insert_with(|| node_ids::turn_id(&uuid()));
        }
    };
    for m in messages {
        turn(&mut ids, &m.info["anchor"]["turnId"]);
        if let Some(product) = m.info["anchor"]["productTurnId"]
            .as_str()
            .filter(|s| !s.is_empty())
        {
            let mapped = ids
                .messages
                .get(product)
                .cloned()
                .unwrap_or_else(|| node_ids::turn_id(&uuid()));
            ids.product_turns.entry(product.into()).or_insert(mapped);
        }
        for part in &m.parts {
            if part["type"] == "timeline" {
                turn(&mut ids, &part["anchorTurnId"]);
                if part["timelineType"] == "goal_verification" {
                    let target = part["targetId"].as_str().unwrap_or("").to_owned();
                    ids.targets
                        .entry(target)
                        .or_insert_with(|| format!("fork_target_{}", uuid()));
                    let verification = part["verificationId"].as_str().unwrap_or("").to_owned();
                    ids.verifications
                        .entry(verification)
                        .or_insert_with(|| format!("fork_verify_{}", uuid()));
                }
            }
            if part["type"] == "compaction" {
                turn(&mut ids, &part["compactBoundary"]["turnId"]);
            }
        }
    }
    for goal in goals {
        let target = goal["targetID"].as_str().unwrap_or("").to_owned();
        ids.targets
            .entry(target)
            .or_insert_with(|| format!("fork_target_{}", uuid()));
    }
    for entry in verifiers {
        ids.verifier_entries
            .insert(entry.id.clone(), format!("fork_goal_verify_{}", uuid()));
        let payload = &entry.data["payload"];
        if let Some(v) = payload["verificationId"].as_str() {
            ids.verifications
                .entry(v.into())
                .or_insert_with(|| format!("fork_verify_{}", uuid()));
        }
        turn(&mut ids, &payload["anchorTurnId"]);
    }
    ids
}

/// The goal snapshots and verifier entry ids the copied transcript references
/// (Node `collectForkGoalSnapshots`, `collectVerifierEntryIds`).
fn goal_references<'a>(
    messages: &'a [Cow<Record>],
    boundary: &'a Value,
) -> (Vec<&'a Value>, Vec<String>) {
    let mut goals: Vec<&Value> = vec![];
    let mut verifiers: Vec<String> = vec![];
    let boundaries = messages
        .iter()
        .map(|m| &m.info["anchor"]["goalBoundary"])
        .chain([boundary]);
    for goal in boundaries.filter(|g| g["kind"] == "snapshot") {
        let id = &goal["target"]["targetID"];
        goals.retain(|g| g["targetID"] != *id);
        goals.push(&goal["target"]);
        for entry in goal["verificationEntryIds"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let entry = entry.as_str().unwrap_or("").to_owned();
            if !verifiers.contains(&entry) {
                verifiers.push(entry);
            }
        }
    }
    (goals, verifiers)
}

/// A Node `SessionGoal` object (a fork's goal snapshot) as a stored target.
fn goal_target(goal: &Value) -> Option<targets::Target> {
    let text = |key: &str| goal[key].as_str().map(str::to_owned);
    Some(targets::Target {
        session_id: text("sessionID")?,
        target_id: text("targetID")?,
        objective: text("objective")?,
        summary_title: text("summaryTitle"),
        status: text("status")?,
        token_budget: goal["tokenBudget"].as_i64(),
        tokens_used: goal["tokensUsed"].as_i64().unwrap_or(0),
        time_used_seconds: goal["timeUsedSeconds"].as_i64().unwrap_or(0),
        active_input_id: None,
        active_run_started_at: None,
        active_run_last_seen_at: None,
        time_created: goal["time"]["created"].as_i64()?,
        time_updated: goal["time"]["updated"].as_i64()?,
    })
}

/// Commits the fork of `request.parent` at its stable boundary as `child`.
pub fn fork(conn: &rusqlite::Connection, child: &str, request: &Value, now: i64) -> Result<()> {
    let parent_id = request["parent"].as_str().context("fork parent")?;
    let boundary = request["boundary"].as_str().context("fork boundary")?;
    let command = request["command"].as_str().context("fork command")?;
    let fact_id = format!("v4_command_fact:child:{parent_id}:{command}");
    if entries::list(conn, parent_id, Some(COMMAND_FACT_ENTRY))?
        .iter()
        .any(|e| e.id == fact_id)
    {
        bail!("Fork command already committed: {fact_id}");
    }
    let parent = sessions::get(conn, parent_id)?.context("Fork parent missing")?;
    let all = cold::records(conn, parent_id)?;
    let branch = Branch::from_revert(parent.revert.as_ref());
    let active = select_branch(all.iter().map(Cow::Borrowed).collect(), &branch);
    let (target, goal_boundary) = target(conn, &active, boundary, now)?;
    let messages = history(&active, &target)?;
    let (goals, verifier_ids) = goal_references(&messages, &goal_boundary);
    let stored = entries::list(conn, parent_id, Some(VERIFICATION_ENTRY))?;
    let verifiers = verifier_ids
        .iter()
        .map(|id| {
            stored
                .iter()
                .find(|e| e.id == *id)
                .cloned()
                .context("Stable fork verifier boundary references missing entries")
        })
        .collect::<Result<Vec<_>>>()?;
    let ids = identities(child, &messages, &goals, &verifiers, now as u64);
    let selection = selection(&messages, &request["selection"]);
    let execution = super::fork_bundle::execution(&messages, &request["execution"]);
    let bundle = super::fork_bundle::Bundle {
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
        save_message(
            conn,
            &clone::message(&m.info, &ids, &next)?,
            source(m.id()),
            now,
        )?;
        for part in &m.parts {
            let copy = clone::part(part, &ids, &next)?;
            save_part(conn, &copy, source(part["id"].as_str().unwrap_or("")), now)?;
        }
    }
    for notice in bundle.notice(boundary) {
        save_message(conn, &notice.info, None, now)?;
        for part in &notice.parts {
            save_part(conn, part, None, now)?;
        }
    }
    if goal_boundary["kind"] == "snapshot" {
        let goal = clone::goal(&goal_boundary["target"], &ids)?;
        let source = goal_target(&goal).context("Stable fork goal snapshot")?;
        let status = goal_boundary["target"]["status"]
            .as_str()
            .unwrap_or("paused");
        targets::clone_for_fork(conn, child, &source, status, now)?;
    }
    for entry in &verifiers {
        entries::save(conn, &bundle.verifier(entry)?)?;
    }
    for entry in bundle.entries() {
        entries::save(conn, &entry)?;
    }
    entries::save(
        conn,
        &bundle.command_fact(&fact_id, request["revision"].as_u64().unwrap_or(0)),
    )?;
    Ok(())
}
