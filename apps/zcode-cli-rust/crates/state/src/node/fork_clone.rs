//! Node `cloneMessageForFork` / `clonePartForFork` with the complete
//! child-local identity map of an atomic fork (`strictLocalReferences`):
//! every parent reference is remapped, or the fork fails.
use anyhow::{Context, Result};
use serde_json::{Map, Value, json};
use std::collections::HashMap;

/// Node `ForkIdentityMap`.
#[derive(Default)]
pub struct Identities {
    pub child: String,
    pub messages: HashMap<String, String>,
    pub parts: HashMap<String, String>,
    pub turns: HashMap<String, String>,
    pub product_turns: HashMap<String, String>,
    pub targets: HashMap<String, String>,
    pub verifier_entries: HashMap<String, String>,
    pub verifications: HashMap<String, String>,
    pub tool_calls: HashMap<String, String>,
}

/// The child-local id of `id`; a parent reference outside the child fails.
pub fn local(map: &HashMap<String, String>, id: &str, field: &str) -> Result<String> {
    map.get(id)
        .cloned()
        .with_context(|| format!("Stable fork cannot remap {field}: {id}"))
}

fn remap(value: &mut Value, map: &HashMap<String, String>, field: &str) -> Result<()> {
    if let Some(id) = value.as_str() {
        *value = local(map, id, field)?.into();
    }
    Ok(())
}

fn remap_list(value: &mut Value, map: &HashMap<String, String>, field: &str) -> Result<()> {
    if let Some(ids) = value.as_array_mut() {
        for id in ids {
            remap(id, map, field)?;
        }
    }
    Ok(())
}

/// Node `remapGoalForFork` / the goal boundary's target.
pub fn goal(target: &Value, ids: &Identities) -> Result<Value> {
    let mut goal = target.clone();
    goal["sessionID"] = ids.child.clone().into();
    let id = target["targetID"].as_str().unwrap_or("");
    goal["targetID"] = local(&ids.targets, id, "goal target")?.into();
    for key in [
        "activeInputId",
        "activeRunStartedAtMs",
        "activeRunLastSeenAtMs",
    ] {
        goal[key] = Value::Null;
    }
    Ok(goal)
}

fn anchor(anchor: &Value, ids: &Identities) -> Result<Value> {
    let mut out = anchor.clone();
    if anchor["turnId"].as_str().is_some_and(|t| !t.is_empty()) {
        remap(&mut out["turnId"], &ids.turns, "anchor turn")?;
    }
    if anchor["productTurnId"]
        .as_str()
        .is_some_and(|t| !t.is_empty())
    {
        remap(
            &mut out["productTurnId"],
            &ids.product_turns,
            "anchor product turn",
        )?;
    }
    if anchor
        .get("orderedMessageIds")
        .is_some_and(|v| !v.is_null())
    {
        remap_list(
            &mut out["orderedMessageIds"],
            &ids.messages,
            "anchor orderedMessageId",
        )?;
    }
    if anchor["boundaryMessageId"]
        .as_str()
        .is_some_and(|t| !t.is_empty())
    {
        remap(
            &mut out["boundaryMessageId"],
            &ids.messages,
            "anchor boundaryMessageId",
        )?;
    }
    let boundary = &anchor["goalBoundary"];
    if boundary["kind"] == "snapshot" {
        let mut entries = boundary["verificationEntryIds"].clone();
        remap_list(&mut entries, &ids.verifier_entries, "goal verifier entry")?;
        out["goalBoundary"] = json!({"kind": "snapshot", "target": goal(&boundary["target"], ids)?,
            "verificationEntryIds": entries});
    }
    Ok(out)
}

/// Node `cloneMessageForFork`: the child-local message with its origin.
pub fn message(info: &Value, ids: &Identities, next: &str) -> Result<Value> {
    let origin = json!({"sessionId": info["sessionID"], "messageId": info["id"]});
    let mut out = info.clone();
    out["id"] = next.into();
    out["sessionID"] = ids.child.clone().into();
    if info["role"] != "user" {
        let parent = info["parentID"].as_str().unwrap_or("");
        out["parentID"] = local(&ids.messages, parent, "assistant parent")?.into();
    }
    if info.get("anchor").is_some_and(|a| a.is_object()) {
        out["anchor"] = anchor(&info["anchor"], ids)?;
    }
    let mut metadata = match info.get("metadata") {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    };
    metadata.insert("forkOrigin".into(), origin);
    out["metadata"] = Value::Object(metadata);
    Ok(out)
}

fn compaction(part: &mut Value, ids: &Identities) -> Result<()> {
    for key in ["summaryMessageId", "tail_start_id"] {
        if part[key].as_str().is_some_and(|s| !s.is_empty()) {
            remap(&mut part[key], &ids.messages, "compaction message")?;
        }
    }
    let Some(boundary) = part.get_mut("compactBoundary").filter(|b| b.is_object()) else {
        return Ok(());
    };
    if boundary["lastSummarizedMessageId"]
        .as_str()
        .is_some_and(|s| !s.is_empty())
    {
        remap(
            &mut boundary["lastSummarizedMessageId"],
            &ids.messages,
            "compact boundary",
        )?;
    }
    remap_list(
        &mut boundary["summaryMessageIds"],
        &ids.messages,
        "compact summary",
    )?;
    for key in ["attachmentMessageIds", "hookResultMessageIds"] {
        if boundary.get(key).is_some_and(|v| !v.is_null()) {
            remap_list(
                &mut boundary[key],
                &ids.messages,
                "compact boundary message",
            )?;
        }
    }
    if let Some(segment) = boundary.get("preservedSegment").filter(|s| s.is_object()) {
        let mut next = Map::new();
        for key in ["headMessageId", "anchorMessageId", "tailMessageId"] {
            let id = segment[key].as_str().unwrap_or("");
            next.insert(
                key.into(),
                local(&ids.messages, id, "compact preserved segment")?.into(),
            );
        }
        boundary["preservedSegment"] = Value::Object(next);
    }
    if boundary["turnId"].as_str().is_some_and(|s| !s.is_empty()) {
        remap(
            &mut boundary["turnId"],
            &ids.turns,
            "compact boundary turnId",
        )?;
    }
    Ok(())
}

/// Node `clonePartForFork`: the child-local part and its embedded references.
pub fn part(part: &Value, ids: &Identities, next_message: &str) -> Result<Value> {
    let mut out = part.clone();
    let id = part["id"].as_str().unwrap_or("");
    out["id"] = local(&ids.parts, id, "part")?.into();
    out["sessionID"] = ids.child.clone().into();
    out["messageID"] = next_message.into();
    match part["type"].as_str() {
        Some("timeline") => {
            if part["anchorMessageId"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
            {
                remap(
                    &mut out["anchorMessageId"],
                    &ids.messages,
                    "timeline anchorMessageId",
                )?;
            }
            if part["anchorTurnId"].as_str().is_some_and(|s| !s.is_empty()) {
                remap(
                    &mut out["anchorTurnId"],
                    &ids.turns,
                    "timeline anchorTurnId",
                )?;
            }
            if part["timelineType"] == "context_compaction"
                && part["summaryMessageId"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
            {
                remap(
                    &mut out["summaryMessageId"],
                    &ids.messages,
                    "timeline summary",
                )?;
            }
            if part["timelineType"] == "goal_verification" {
                remap(
                    &mut out["targetId"],
                    &ids.targets,
                    "goal verification target",
                )?;
                remap(
                    &mut out["verificationId"],
                    &ids.verifications,
                    "goal verification",
                )?;
            }
        }
        Some("compaction") => compaction(&mut out, ids)?,
        Some("tool") => {
            remap(&mut out["callID"], &ids.tool_calls, "tool call")?;
            if part["state"]["status"] == "completed"
                && let Some(list) = out["state"]["attachments"].as_array_mut()
            {
                for attachment in list {
                    let id = attachment["id"].as_str().unwrap_or("");
                    attachment["id"] = local(&ids.parts, id, "tool attachment")?.into();
                    attachment["sessionID"] = ids.child.clone().into();
                    attachment["messageID"] = next_message.into();
                }
            }
        }
        _ => {}
    }
    Ok(out)
}
