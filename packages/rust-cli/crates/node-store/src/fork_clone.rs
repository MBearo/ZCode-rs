//! Node `/fork` 的行克隆与目标状态（docs/specs/rust-cli/session-lifecycle.md §6.5.2 的映射表）。
//!
//! 按 Node legacy fork 的规则（`helpers/steering.ts` 的 `cloneMessageForFork` / `clonePartForFork`，只给出消息 ID
//! 映射；`session-fork.ts` 的 `copyGoalStateForFork`）：映射按复制顺序逐条加入，所以指向后面消息的引用保持原值。
//! 对象成员的位置按 JS 展开：已有的成员原地换值，新成员追加在后，删掉的成员不留位置。
use std::collections::HashMap;

use rusqlite::{OptionalExtension, params};
use rust_cli_kernel_api::store::StoreError;
use serde_json::{Map, Value, json};

use crate::OrStorage;
use crate::facts::Commit;
use crate::ids::{message_id, part_id};
use crate::repo::entries::{self, Entry};
use crate::repo::messages::{self, WithParts};

/// 父会话的消息 ID → 新会话的消息 ID。
pub(crate) type Mapping = HashMap<String, String>;

const VERIFICATION_ENTRY: &str = "target_completion_verification";

fn object(value: &Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

/// 能映射就换成新 ID，否则保持原值（legacy，`strictLocalReferences` 为假）。
fn mapped(map: &Mapping, value: &Value) -> Value {
    value
        .as_str()
        .and_then(|id| map.get(id))
        .map_or_else(|| value.clone(), |id| Value::from(id.as_str()))
}

fn mapped_list(map: &Mapping, value: &Value) -> Value {
    match value.as_array() {
        Some(ids) => Value::Array(ids.iter().map(|id| mapped(map, id)).collect()),
        None => value.clone(),
    }
}

fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null | Value::Bool(false)) => false,
        Some(Value::String(text)) => !text.is_empty(),
        Some(_) => true,
    }
}

/// Node `cloneMessageAnchorForFork`（legacy）。
fn anchor(anchor: &Value, map: &Mapping, child: &str) -> Value {
    let mut out = object(anchor);
    if truthy(anchor.get("orderedMessageIds")) {
        out.insert(
            "orderedMessageIds".into(),
            mapped_list(map, &anchor["orderedMessageIds"]),
        );
    }
    if truthy(anchor.get("boundaryMessageId")) {
        out.insert(
            "boundaryMessageId".into(),
            mapped(map, &anchor["boundaryMessageId"]),
        );
    }
    let boundary = &anchor["goalBoundary"];
    if boundary["kind"] == "snapshot" {
        let mut target = object(&boundary["target"]);
        target.insert("sessionID".into(), child.into());
        for key in [
            "activeInputId",
            "activeRunStartedAtMs",
            "activeRunLastSeenAtMs",
        ] {
            target.insert(key.into(), Value::Null);
        }
        out.insert(
            "goalBoundary".into(),
            json!({"kind": "snapshot", "target": target,
                "verificationEntryIds": boundary["verificationEntryIds"]}),
        );
    }
    Value::Object(out)
}

/// Node `cloneMessageForFork`（legacy）。
fn message(info: &Value, map: &Mapping, child: &str, next: &str) -> Value {
    let mut out = object(info);
    let origin = json!({"sessionId": info["sessionID"], "messageId": info["id"]});
    out.insert("id".into(), next.into());
    out.insert("sessionID".into(), child.into());
    if info["role"] != "user" {
        out.insert("parentID".into(), mapped(map, &info["parentID"]));
    }
    if let Some(value) = info.get("anchor").filter(|value| truthy(Some(value))) {
        out.insert("anchor".into(), anchor(value, map, child));
    }
    let mut metadata = object(&info["metadata"]);
    metadata.insert("forkOrigin".into(), origin);
    out.insert("metadata".into(), Value::Object(metadata));
    Value::Object(out)
}

/// `compactBoundary` 的消息 ID 映射（Node `clonePartForFork` 的 compaction 分支）。
fn boundary(boundary: &Value, map: &Mapping) -> Value {
    let mut out = object(boundary);
    if truthy(boundary.get("lastSummarizedMessageId")) {
        out.insert(
            "lastSummarizedMessageId".into(),
            mapped(map, &boundary["lastSummarizedMessageId"]),
        );
    }
    out.insert(
        "summaryMessageIds".into(),
        mapped_list(map, &boundary["summaryMessageIds"]),
    );
    for key in ["attachmentMessageIds", "hookResultMessageIds"] {
        if truthy(boundary.get(key)) {
            out.insert(key.into(), mapped_list(map, &boundary[key]));
        }
    }
    if let Some(segment) = boundary.get("preservedSegment").filter(|v| truthy(Some(v))) {
        out.insert(
            "preservedSegment".into(),
            json!({
                "headMessageId": mapped(map, &segment["headMessageId"]),
                "anchorMessageId": mapped(map, &segment["anchorMessageId"]),
                "tailMessageId": mapped(map, &segment["tailMessageId"]),
            }),
        );
    }
    Value::Object(out)
}

/// Node `clonePartForFork`（legacy：只有消息 ID 映射）。
fn part(ctx: &Commit<'_>, part: &Value, map: &Mapping, next: &str) -> Value {
    let new_id = || part_id(ctx.now, &(ctx.uuid)());
    let mut out = object(part);
    out.insert("id".into(), new_id().into());
    out.insert("sessionID".into(), ctx.session.into());
    out.insert("messageID".into(), next.into());
    match part["type"].as_str() {
        Some("timeline") => {
            if let Some(anchor) = part.get("anchorMessageId").filter(|v| truthy(Some(v))) {
                match anchor.as_str().and_then(|id| map.get(id)) {
                    Some(id) => {
                        out.insert("anchorMessageId".into(), id.as_str().into());
                    }
                    None => {
                        out.shift_remove("anchorMessageId");
                        out.insert("originAnchorMessageId".into(), anchor.clone());
                    }
                }
            }
            // legacy fork 没有轮次映射：只能降级成来源记录。
            if let Some(turn) = part.get("anchorTurnId").filter(|v| truthy(Some(v))) {
                out.shift_remove("anchorTurnId");
                out.insert("originAnchorTurnId".into(), turn.clone());
            }
            if part["timelineType"] == "context_compaction" && truthy(part.get("summaryMessageId"))
            {
                out.insert(
                    "summaryMessageId".into(),
                    mapped(map, &part["summaryMessageId"]),
                );
            }
        }
        Some("compaction") => {
            for key in ["summaryMessageId", "tail_start_id"] {
                if truthy(part.get(key)) {
                    out.insert(key.into(), mapped(map, &part[key]));
                }
            }
            if let Some(value) = part.get("compactBoundary").filter(|v| truthy(Some(v))) {
                out.insert("compactBoundary".into(), boundary(value, map));
            }
        }
        Some("tool") if part["state"]["status"] == "completed" => {
            if let Some(attachments) = part["state"]["attachments"].as_array() {
                let mut state = object(&part["state"]);
                let cloned = attachments
                    .iter()
                    .map(|attachment| {
                        let mut copy = object(attachment);
                        copy.insert("id".into(), new_id().into());
                        copy.insert("sessionID".into(), ctx.session.into());
                        copy.insert("messageID".into(), next.into());
                        Value::Object(copy)
                    })
                    .collect();
                state.insert("attachments".into(), Value::Array(cloned));
                out.insert("state".into(), Value::Object(state));
            }
        }
        _ => {}
    }
    Value::Object(out)
}

/// Node `copySessionMessagesForFork`：逐条换新 ID 写进新会话，返回消息 ID 映射。
pub(crate) fn copy_messages(
    ctx: &Commit<'_>,
    records: &[&WithParts],
) -> Result<Mapping, StoreError> {
    let mut map = Mapping::new();
    for record in records {
        let next = message_id(ctx.now, &(ctx.uuid)());
        map.insert(record.id().to_owned(), next.clone());
        let info = message(&record.info, &map, ctx.session, &next);
        messages::save_message(ctx.conn, &info, ctx.now as i64).storage()?;
        for source in &record.parts {
            let cloned = part(ctx, source, &map, &next);
            messages::save_part(ctx.conn, &cloned, ctx.now as i64).storage()?;
        }
    }
    Ok(map)
}

/// Node `cloneGoalVerificationEntryForFork`：锚定的 assistant 不在复制范围里的跳过。
fn verification(
    ctx: &Commit<'_>,
    entry: &Entry,
    map: &Mapping,
    target: &str,
) -> Option<(Entry, Value)> {
    let payload = &entry.data["payload"];
    if payload["targetId"] != target {
        return None;
    }
    let anchor = payload["anchorAssistantMessageId"].as_str();
    if anchor.is_some_and(|id| !map.contains_key(id)) {
        return None;
    }
    let mut cloned = object(payload);
    if let Some(id) = anchor.and_then(|id| map.get(id)) {
        cloned.insert("anchorAssistantMessageId".into(), id.as_str().into());
    }
    if let Some(turn) = payload["anchorTurnId"].as_str() {
        cloned.shift_remove("anchorTurnId");
        cloned.insert("originAnchorTurnId".into(), turn.into());
    }
    let event = (ctx.uuid)();
    let mut data = object(&entry.data);
    data.insert("eventId".into(), event.as_str().into());
    data.insert("payload".into(), Value::Object(cloned.clone()));
    let copy = Entry {
        id: format!("fork_goal_verify_{event}"),
        session_id: ctx.session.to_owned(),
        data: Value::Object(data),
        touch_session: true,
        ..entry.clone()
    };
    Some((copy, Value::Object(cloned)))
}

/// Node `deriveForkedGoalStatusFromCopiedVerifications`。
fn forked_status(parent: &str, copied: &[Value]) -> String {
    let latest = copied
        .iter()
        .rev()
        .find(|payload| payload["status"] == "completed" && truthy(payload.get("verification")));
    if latest.is_some_and(|payload| payload["verification"]["passed"] == true) {
        return "complete".to_owned();
    }
    if parent == "complete" {
        return "active".to_owned();
    }
    parent.to_owned()
}

/// Node `copyGoalStateForFork`（legacy，没有目标边界）：复制验证记录，再按 `cloneSessionTargetForFork` 写
/// `session_target`。原会话没有目标时什么都不写。
pub(crate) fn copy_goal(ctx: &Commit<'_>, parent: &str, map: &Mapping) -> Result<(), StoreError> {
    let row = ctx
        .conn
        .query_row(
            "select target_id, objective, summary_title, status, token_budget, tokens_used,
               time_used_seconds, time_created, time_updated
             from session_target where session_id = ?",
            [parent],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                ))
            },
        )
        .optional()
        .storage()?;
    let Some((target, objective, summary, status, budget, used, seconds, created, updated)) = row
    else {
        return Ok(());
    };
    let mut copied = Vec::new();
    for entry in entries::list(ctx.conn, parent, Some(VERIFICATION_ENTRY)).storage()? {
        if let Some((copy, payload)) = verification(ctx, &entry, map, &target) {
            entries::save(ctx.conn, &copy).storage()?;
            copied.push(payload);
        }
    }
    ctx.conn
        .execute(
            "insert into session_target (
               session_id, target_id, objective, summary_title, status, token_budget, tokens_used,
               time_used_seconds, active_input_id, active_run_started_at, active_run_last_seen_at,
               time_created, time_updated
             ) values (?, ?, ?, ?, ?, ?, ?, ?, null, null, null, ?, ?)",
            params![
                ctx.session,
                target,
                objective,
                summary,
                forked_status(&status, &copied),
                budget,
                used,
                seconds,
                created,
                updated
            ],
        )
        .storage()?;
    ctx.conn
        .execute(
            "update session set time_updated = max(time_updated, ?) where id = ?",
            params![ctx.now as i64, ctx.session],
        )
        .storage()?;
    Ok(())
}

#[cfg(test)]
#[path = "fork_clone_tests.rs"]
mod tests;
