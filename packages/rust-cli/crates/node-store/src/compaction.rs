//! 压缩的落库与冷读取（docs/specs/rust-cli/compact.md §6）。
//!
//! 内核的边界（`System(CompactBoundary)`）与摘要（`User(CompactSummary)`）在同一批事实里先后到达：
//! - 边界先存成 `rust-cli/compact_boundary` entry（只供同一事务里的摘要取用，Node 忽略）；
//! - 摘要按 Node `persistCompactSummary`（`compact-persistence.ts`）写成带 `summary` 的 user 消息，
//!   part 是一个 synthetic text 与一个 `compaction`，`compactBoundary` 只写 Node strict schema
//!   （`contracts/src/compact/index.ts`）里的字段。
//!
//! 冷读取：带 `summary` 且有 `compactBoundary` 的 user 消息还原成边界 + 摘要；Node 自己写的同样能读。
use rusqlite::{OptionalExtension, params};
use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::{
    CompactBoundary, CompactTrigger, ContentBlock, Message, PreservedSegment, SystemMessage,
    UserMessage,
};
use rust_cli_kernel_api::store::StoreError;
use serde_json::{Value, json};

use crate::facts::{Commit, save_entry};
use crate::ids::{created_at, part_id};
use crate::records::{self, AGENT};
use crate::repo::{messages, sessions};
use crate::{OrStorage, state, storage};

/// entry 种类（不带 `rust-cli/` 前缀的部分）。
const BOUNDARY_ENTRY: &str = "rust-cli/compact_boundary";

fn entry_id(session: &str, boundary: &str) -> String {
    format!("{session}:{BOUNDARY_ENTRY}:{boundary}")
}

/// trigger → Node 的 `trigger`、`phase`、`compactReason`。
fn node_trigger(trigger: CompactTrigger) -> (&'static str, &'static str, &'static str) {
    match trigger {
        CompactTrigger::Manual => ("manual", "standalone_turn", "user_requested"),
        CompactTrigger::Auto => ("auto", "pre_request", "context_limit"),
        CompactTrigger::Reactive => ("reactive", "reactive", "provider_overflow"),
    }
}

fn kernel_trigger(trigger: &str) -> CompactTrigger {
    match trigger {
        "manual" => CompactTrigger::Manual,
        "reactive" => CompactTrigger::Reactive,
        _ => CompactTrigger::Auto,
    }
}

pub fn append_boundary(ctx: &Commit<'_>, boundary: &CompactBoundary) -> Result<(), StoreError> {
    let data = serde_json::to_value(boundary).map_err(|error| storage(error.to_string()))?;
    let entry = records::entry(
        entry_id(ctx.session, boundary.id.as_str()),
        ctx.session,
        BOUNDARY_ENTRY,
        ctx.now,
        data,
        false,
    );
    save_entry(ctx, &entry)
}

/// 同一事务里先到的边界（摘要 ID 对得上的最后一个）。
fn boundary_for(ctx: &Commit<'_>, summary: &str) -> Result<CompactBoundary, StoreError> {
    let entries =
        crate::repo::entries::list(ctx.conn, ctx.session, Some(BOUNDARY_ENTRY)).storage()?;
    entries
        .into_iter()
        .rev()
        .filter_map(|entry| serde_json::from_value::<CompactBoundary>(entry.data).ok())
        .find(|boundary| boundary.summary.as_str() == summary)
        .ok_or_else(|| {
            storage(format!(
                "compact summary {summary} has no boundary before it"
            ))
        })
}

/// 有 message 行的 ID：工具结果消息没有行（存在 assistant 的 tool part 里），换成它的 assistant。
fn row_id(ctx: &Commit<'_>, id: &str) -> Result<String, StoreError> {
    if messages::exists(ctx.conn, id).storage()? {
        return Ok(id.to_owned());
    }
    ctx.conn
        .query_row(
            "select message_id from part where session_id = ?
              and json_extract(data, '$.metadata.rustCli.resultMessageId') = ? limit 1",
            params![ctx.session, id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .storage()?
        .ok_or_else(|| storage(format!("preserved message {id} has no stored row")))
}

/// `compactBoundary`（Node strict schema 的字段，键序同 schema）。
fn payload(
    ctx: &Commit<'_>,
    boundary: &CompactBoundary,
    summary: &str,
) -> Result<Value, StoreError> {
    let (trigger, phase, reason) = node_trigger(boundary.trigger);
    let rows: Vec<String> = messages::messages(ctx.conn, ctx.session)
        .storage()?
        .iter()
        .map(|record| record.id().to_owned())
        .collect();
    let segment = match &boundary.preserved {
        Some(segment) => {
            let head = row_id(ctx, segment.head.as_str())?;
            let tail = row_id(ctx, segment.tail.as_str())?;
            let position = |id: &str| rows.iter().position(|row| row == id);
            let kept = match (position(&head), position(&tail)) {
                (Some(start), Some(end)) if end >= start => end - start + 1,
                _ => return Err(storage("preserved segment is not a stored range")),
            };
            Some((head, tail, kept))
        }
        None => None,
    };
    let kept = segment.as_ref().map_or(0, |(_, _, kept)| *kept);
    let row = sessions::get(ctx.conn, ctx.session)
        .storage()?
        .ok_or_else(|| storage(format!("session {} disappeared", ctx.session)))?;
    let mut out = json!({
        "boundaryId": boundary.id.as_str(),
        "trigger": trigger,
        "phase": phase,
        "compactReason": reason,
        "summarySource": "model",
        "preCompactTokenCount": boundary.pre_tokens,
        "summarizedMessageCount": rows.len().saturating_sub(kept),
    });
    if let Some((head, tail, kept)) = segment {
        out["keptMessageCount"] = kept.into();
        out["preservedSegment"] =
            json!({"headMessageId": head, "anchorMessageId": summary, "tailMessageId": tail});
    }
    out["summaryMessageIds"] = json!([summary]);
    out["traceId"] = row
        .trace_id
        .unwrap_or_else(|| ctx.session.to_owned())
        .into();
    Ok(out)
}

/// Node `persistCompactSummary` 的消息与 part。
pub fn append_summary(ctx: &Commit<'_>, user: &UserMessage) -> Result<(), StoreError> {
    let text = match user.content.as_slice() {
        [ContentBlock::Text { text }] => text.as_str(),
        _ => {
            return Err(crate::facts::unsupported(
                "compact summary with non-text blocks",
            ));
        }
    };
    let id = user.id.as_str();
    let boundary = boundary_for(ctx, id)?;
    let compact_boundary = payload(ctx, &boundary, id)?;
    let created = created_at(id, ctx.now);
    let mut info = json!({"id": id, "sessionID": ctx.session, "role": "user",
        "time": {"created": created},
        "summary": {"title": "Compact summary", "body": text, "diffs": []},
        "agent": AGENT});
    if let Some(selection) = state::selection(ctx.conn, ctx.session).storage()? {
        info["modelSelection"] =
            json!({"providerId": selection["providerId"], "modelId": selection["modelId"]});
    }
    info["semantics"] = json!({"origin": "agent_runtime", "kind": "compact_summary",
        "uiVisibility": "hidden", "providerVisibility": "visible", "transcriptVisibility": "hidden"});
    info["tools"] = json!({});
    messages::save_message(ctx.conn, &info, ctx.now as i64).storage()?;
    let text_part = json!({"id": part_id(ctx.now, &(ctx.uuid)()), "sessionID": ctx.session,
        "messageID": id, "type": "text", "text": text, "synthetic": true,
        "time": {"start": created, "end": created}});
    messages::save_part(ctx.conn, &text_part, ctx.now as i64).storage()?;
    let (trigger, phase, reason) = node_trigger(boundary.trigger);
    let compaction = json!({"id": part_id(ctx.now, &(ctx.uuid)()), "sessionID": ctx.session,
        "messageID": id, "type": "compaction", "auto": trigger == "auto", "trigger": trigger,
        "phase": phase, "compactReason": reason, "compactBoundary": compact_boundary});
    messages::save_part(ctx.conn, &compaction, ctx.now as i64).storage()?;
    Ok(())
}

/// 冷读取：摘要消息的 `compactBoundary` → 内核边界（摘要之前）。
pub fn hydrate_boundary(parts: &[Value], summary: &str) -> Option<Message> {
    let payload = parts
        .iter()
        .find(|part| part["type"] == "compaction" && part["compactBoundary"].is_object())
        .map(|part| &part["compactBoundary"])?;
    let preserved = payload["preservedSegment"].as_object().and_then(|segment| {
        let id = |key: &str| segment.get(key)?.as_str().map(MessageId::new);
        Some(PreservedSegment {
            head: id("headMessageId")?,
            anchor: id("anchorMessageId")?,
            tail: id("tailMessageId")?,
        })
    });
    Some(Message::System(SystemMessage::CompactBoundary(
        CompactBoundary {
            id: MessageId::new(payload["boundaryId"].as_str()?),
            trigger: kernel_trigger(payload["trigger"].as_str().unwrap_or("")),
            pre_tokens: payload["preCompactTokenCount"].as_u64().unwrap_or(0),
            summary: MessageId::new(payload["summaryMessageIds"][0].as_str().unwrap_or(summary)),
            preserved,
        },
    )))
}

#[cfg(test)]
#[path = "compaction_tests.rs"]
mod tests;
