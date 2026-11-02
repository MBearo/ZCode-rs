//! 压缩的落库与冷读取（docs/specs/rust-cli/compact.md §6）。
//!
//! 内核的边界（`System(CompactBoundary)`）与摘要（`User(CompactSummary)`）在同一批事实里先后到达：
//! - 边界先存成 `rust-cli/compact_boundary` entry（只供同一事务里的摘要取用，Node 忽略）；
//! - 摘要按 Node `persistCompactSummary`（`compact-persistence.ts`）写成带 `summary` 的 user 消息，
//!   part 是一个 synthetic text 与一个 `compaction`，`compactBoundary` 只写 Node strict schema
//!   （`contracts/src/compact/index.ts`）里的字段。
//!
//! 压缩前已加载的工具（compact.md §3.5）写在 `compaction` part 的 `metadata.rustCli`（不进 strict 的 `compactBoundary`）。
//!
//! 冷读取：带 `summary` 且有 `compactBoundary` 的 user 消息还原成边界 + 摘要；Node 自己写的同样能读（node-records 的
//! `compaction`，F128）。
use rust_cli_kernel_api::message::{CompactBoundary, ContentBlock, UserMessage};
use rust_cli_kernel_api::store::StoreError;
use rust_cli_node_records::compaction::{DISCOVERED_KEY, node_trigger};
use rust_cli_node_records::records::RUST_CLI;
use serde_json::{Value, json};

use crate::facts::{Commit, save_entry};
use crate::ids::{created_at, part_id};
use crate::keys::{self, EntryKey};
use crate::records::{self, AGENT};
use crate::repo::{messages, sessions};
use crate::{OrStorage, state, storage};

pub fn append_boundary(ctx: &Commit<'_>, boundary: &CompactBoundary) -> Result<(), StoreError> {
    let data = serde_json::to_value(boundary).map_err(|error| storage(error.to_string()))?;
    let entry = records::entry(
        EntryKey::item(ctx.session, keys::COMPACT_BOUNDARY, boundary.id.as_str()).encode(),
        ctx.session,
        &keys::named(keys::COMPACT_BOUNDARY),
        ctx.now,
        data,
        false,
    );
    save_entry(ctx, &entry)
}

/// 同一事务里先到的边界（摘要 ID 对得上的最后一个）。
fn boundary_for(ctx: &Commit<'_>, summary: &str) -> Result<CompactBoundary, StoreError> {
    let entries = crate::repo::entries::list(
        ctx.conn,
        ctx.session,
        Some(&keys::named(keys::COMPACT_BOUNDARY)),
    )
    .storage()?;
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

/// 有 message 行的 ID：工具结果消息没有行（存在 assistant 的 tool part 里），换成它的 assistant。本工程写的结果按
/// `resultMessageId` 找；Node 写的结果与冷读取给 Node 的未结调用补在内存里的结果用 part id 作消息 id，按 part id 找。
/// 修 bug（工作线 F86）：以前只认 `resultMessageId`，保留段的首尾是这两种结果时提交报 `has no stored row`。
fn row_id(ctx: &Commit<'_>, id: &str) -> Result<String, StoreError> {
    if messages::exists(ctx.conn, id).storage()? {
        return Ok(id.to_owned());
    }
    messages::message_of_result(ctx.conn, ctx.session, id)
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
    let mut compaction = json!({"id": part_id(ctx.now, &(ctx.uuid)()), "sessionID": ctx.session,
        "messageID": id, "type": "compaction", "auto": trigger == "auto", "trigger": trigger,
        "phase": phase, "compactReason": reason, "compactBoundary": compact_boundary});
    // 压缩前已加载的工具（compact.md §3.5）：`compactBoundary` 是 strict，放在 part 的 `metadata.rustCli`。
    if !boundary.discovered_tools.is_empty() {
        compaction["metadata"] = json!({ RUST_CLI: { DISCOVERED_KEY: boundary.discovered_tools } });
    }
    messages::save_part(ctx.conn, &compaction, ctx.now as i64).storage()?;
    Ok(())
}

#[cfg(test)]
#[path = "compaction_tests.rs"]
mod tests;
