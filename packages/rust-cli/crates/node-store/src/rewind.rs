//! 回退与 checkpoint 的 Node 记录（docs/specs/rust-cli/session-lifecycle.md §4.2、§5.3）。
//!
//! - `RevertSet` → Node `setRevert`：保留前缀、切点与代数按 Node `buildConversationRewindPlan`
//!   从库里算（库是唯一事实源；工具结果没有 message 行，压缩前的旧行内核不在内存里）。
//! - `CheckpointCreated` → `runtime/workspace_checkpoint`（Node `persistWorkspaceCheckpointEntry`）。
//! - `CheckpointsRestored` → 每个受影响的回合一条 `runtime/workspace_file_rewind`
//!   （Node `persistWorkspaceFileRewindEntry`，`reason = file_summary_rewind`）。
//! - 远端机器上的 checkpoint（R3，docs/specs/rust-cli/remote-exec.md §17.1、§20.9）：同样的形状，类型换成本工程的
//!   `rust-cli/remote_checkpoint` 与 `rust-cli/remote_file_rewind`，`rustCli` 里多 `machine`。Node / App 按类型查询，看不到
//!   它们，App 回退不会把远端文件的内容写到本机的同名路径；序号在这两种记录之间单独算。
use std::borrow::Cow;

use rusqlite::Connection;
use rust_cli_kernel_api::ids::{ArtifactId, MessageId, SessionId, ToolUseId};
use rust_cli_kernel_api::store::{ArtifactRef, RevertCursor, StoreError, WorkspaceCheckpoint};
use serde_json::{Value, json};

use crate::branch::{Branch, select_branch};
use crate::facts::{Commit, save_entry};
use crate::repo::entries;
use crate::repo::messages;
use crate::repo::sessions;
use crate::state;
use crate::{OrStorage, records, storage};

pub const CHECKPOINT_ENTRY: &str = "runtime/workspace_checkpoint";
pub const FILE_REWIND_ENTRY: &str = "runtime/workspace_file_rewind";
/// 远端机器上的 checkpoint 与文件回退（R3）。
pub const REMOTE_CHECKPOINT_ENTRY: &str = "rust-cli/remote_checkpoint";
pub const REMOTE_FILE_REWIND_ENTRY: &str = "rust-cli/remote_file_rewind";

/// 一种记录：Node 的（本机）或本工程的（远端机器）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Node,
    Remote,
}

impl Kind {
    fn of(machine: Option<&str>) -> Self {
        if machine.is_some() {
            Self::Remote
        } else {
            Self::Node
        }
    }

    fn checkpoint(self) -> &'static str {
        match self {
            Self::Node => CHECKPOINT_ENTRY,
            Self::Remote => REMOTE_CHECKPOINT_ENTRY,
        }
    }

    fn file_rewind(self) -> &'static str {
        match self {
            Self::Node => FILE_REWIND_ENTRY,
            Self::Remote => REMOTE_FILE_REWIND_ENTRY,
        }
    }
}

/// Node `setRevert` 的值（键序同 Node）。
pub fn revert(ctx: &Commit<'_>, cursor: &RevertCursor) -> Result<Value, StoreError> {
    let row = sessions::get(ctx.conn, ctx.session)
        .storage()?
        .ok_or_else(|| storage(format!("session {} disappeared", ctx.session)))?;
    let records = messages::messages(ctx.conn, ctx.session).storage()?;
    let branch = Branch::from_revert(row.revert.as_ref());
    // 只应用旧游标、不应用压缩边界：目标可以在压缩边界之前（Node 同样处理）。
    let active = select_branch(records.iter().map(Cow::Borrowed).collect(), &branch);
    let target = cursor.target.as_str();
    let index = active
        .iter()
        .position(|record| record.id() == target)
        .ok_or_else(|| {
            storage(format!(
                "rewind target {target} is not in the active branch"
            ))
        })?;
    let kept: Vec<&str> = active[..index].iter().map(|record| record.id()).collect();
    let cut = records
        .last()
        .map(messages::WithParts::id)
        .ok_or_else(|| storage("rewind needs at least one message"))?;
    let previous = row
        .revert
        .as_ref()
        .and_then(|value| value["branchGeneration"].as_u64())
        .unwrap_or(0);
    Ok(json!({
        "keptMessageIDs": kept,
        "branchCutAfterMessageID": cut,
        "branchGeneration": (previous + 1).max(cursor.generation),
        "messageID": kept.last().copied().unwrap_or(target),
        "kind": "conversation_rewind",
        "scope": "conversation",
        "targetMessageID": target,
    }))
}

/// 工作区事件的序号（Node `sequenceNumber`，Node 恢复时按它排序、App 按它取增量）：本会话已持久化的
/// checkpoint 与文件回退记录（Node 与本工程写的）里的最大值加一。同一事务里先写的记录立刻可见，所以同一批的
/// 几个 checkpoint 依次加一。修复线 F2：以前用本工程的提交序号（`rust-cli/commit`），与 Node 的事件序号不是一个
/// 空间：混写时顺序错、会重复，同一批的几个 checkpoint 序号也相同。
fn sequence(ctx: &Commit<'_>, kind: Kind) -> Result<u64, StoreError> {
    let max: Option<i64> = ctx
        .conn
        .prepare_cached(
            "select max(cast(json_extract(data, '$.sequenceNumber') as integer)) from session_entry
             where session_id = ? and type in (?, ?)",
        )
        .storage()?
        .query_row(
            rusqlite::params![ctx.session, kind.checkpoint(), kind.file_rewind()],
            |row| row.get(0),
        )
        .storage()?;
    Ok(max.map_or(0, |max| u64::try_from(max).unwrap_or(0)) + 1)
}

fn trace(ctx: &Commit<'_>) -> Result<String, StoreError> {
    let row = sessions::get(ctx.conn, ctx.session).storage()?;
    Ok(row
        .and_then(|row| row.trace_id)
        .unwrap_or_else(|| ctx.session.to_owned()))
}

/// Node 的事件包装（`PersistedWorkspaceEvent`）；`turnId` 取 user 消息的 `anchor.turnId`。
fn persisted(
    ctx: &Commit<'_>,
    payload: Value,
    prompt: &str,
    kind: Kind,
) -> Result<Value, StoreError> {
    let mut data = json!({
        "eventId": (ctx.uuid)(),
        "payload": payload,
        "sequenceNumber": sequence(ctx, kind)?,
        "traceId": trace(ctx)?,
    });
    if let Some(turn) = state::message(ctx.conn, ctx.session, prompt)
        .storage()?
        .and_then(|info| info["anchor"]["turnId"].as_str().map(str::to_owned))
    {
        data["turnId"] = turn.into();
    }
    Ok(data)
}

pub fn checkpoint(ctx: &Commit<'_>, checkpoint: &WorkspaceCheckpoint) -> Result<(), StoreError> {
    let uri = crate::artifact_uri(&checkpoint.snapshot);
    let payload = json!({
        "checkpointId": checkpoint.id,
        "messageId": checkpoint.prompt.as_str(),
        "targetMessageId": checkpoint.prompt.as_str(),
        "toolMessageId": checkpoint.assistant.as_str(),
        "scope": "workspace",
        "snapshotRef": uri,
        "diffRef": uri,
        "fileCount": 1,
    });
    let kind = Kind::of(checkpoint.machine.as_deref());
    let mut data = persisted(ctx, payload, checkpoint.prompt.as_str(), kind)?;
    // payload 是 Node 的 strict schema；内核读回要的 Artifact 引用与调用 id 放在 payload 之外。
    data["rustCli"] = json!({
        "snapshot": checkpoint.snapshot,
        "toolUse": checkpoint.tool_use.as_str(),
    });
    if let Some(machine) = &checkpoint.machine {
        data["rustCli"]["machine"] = Value::from(machine.as_str());
    }
    let event = data["eventId"].as_str().unwrap_or_default().to_owned();
    let id = match kind {
        Kind::Node => format!("workspace-checkpoint:{event}"),
        Kind::Remote => format!("rust-cli-remote-checkpoint:{event}"),
    };
    let entry = records::entry(id, ctx.session, kind.checkpoint(), ctx.now, data, true);
    save_entry(ctx, &entry)
}

/// 每个受影响的回合（同一 `messageId`）一条文件回退记录，指向该回合最后一个被撤销的 checkpoint。本机与远端的分开写
/// （各自的记录类型）。
pub fn restored(ctx: &Commit<'_>, ids: &[String]) -> Result<(), StoreError> {
    for kind in [Kind::Node, Kind::Remote] {
        restored_of(ctx, ids, kind)?;
    }
    Ok(())
}

fn restored_of(ctx: &Commit<'_>, ids: &[String], kind: Kind) -> Result<(), StoreError> {
    let all = stored(ctx.conn, ctx.session, kind)?;
    let mut turns: Vec<(&str, &Stored)> = Vec::new();
    for checkpoint in all.iter().filter(|c| ids.contains(&c.id)) {
        match turns
            .iter_mut()
            .find(|(prompt, _)| *prompt == checkpoint.prompt)
        {
            Some(turn) => turn.1 = checkpoint,
            None => turns.push((&checkpoint.prompt, checkpoint)),
        }
    }
    for (prompt, last) in turns {
        let rewind = format!("rewind_{}", (ctx.uuid)());
        let payload = json!({
            "rewindId": rewind,
            "scope": "workspace",
            "strategy": "active_chain",
            "targetMessageId": prompt,
            "targetCheckpointId": last.id,
            "restoredSnapshotRef": last.uri,
            "reason": "file_summary_rewind",
        });
        let data = persisted(ctx, payload, prompt, kind)?;
        let id = match kind {
            Kind::Node => format!("workspace-file-rewind:{rewind}"),
            Kind::Remote => format!("rust-cli-remote-file-rewind:{rewind}"),
        };
        let entry = records::entry(id, ctx.session, kind.file_rewind(), ctx.now, data, true);
        save_entry(ctx, &entry)?;
    }
    Ok(())
}

struct Stored {
    id: String,
    prompt: String,
    uri: String,
    sequence: u64,
    kernel: Option<Active>,
}

/// 冷读取给出的一个 checkpoint：本工程写的已是内核的值；Node 写的还要按产物文件补出引用（`node_checkpoints.rs`）。
#[derive(Debug, Clone, PartialEq)]
pub enum Active {
    Rust(WorkspaceCheckpoint),
    Node(NodeCheckpoint),
}

/// Node `persistWorkspaceCheckpointEntry` 的记录（没有 `data.rustCli`）：引用只有 `snapshotRef` 的 URI。
#[derive(Debug, Clone, PartialEq)]
pub struct NodeCheckpoint {
    pub id: String,
    pub prompt: MessageId,
    pub assistant: MessageId,
    pub session: SessionId,
    pub artifact: ArtifactId,
}

fn stored(conn: &Connection, session: &str, kind: Kind) -> Result<Vec<Stored>, StoreError> {
    let entries = entries::list(conn, session, Some(kind.checkpoint())).storage()?;
    let mut out: Vec<Stored> = entries
        .into_iter()
        .filter_map(|entry| {
            let data = entry.data;
            let payload = &data["payload"];
            let id = payload["checkpointId"].as_str()?.to_owned();
            let prompt = payload["targetMessageId"]
                .as_str()
                .or_else(|| payload["messageId"].as_str())?
                .to_owned();
            let assistant = payload["toolMessageId"].as_str().map(MessageId::new);
            let snapshot: Option<ArtifactRef> =
                serde_json::from_value(data["rustCli"]["snapshot"].clone()).ok();
            let tool_use = data["rustCli"]["toolUse"].as_str().map(ToolUseId::new);
            let machine = data["rustCli"]["machine"].as_str().map(str::to_owned);
            let uri = payload["snapshotRef"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let kernel = match (assistant, snapshot, tool_use) {
                (Some(assistant), Some(snapshot), Some(tool_use)) => {
                    Some(Active::Rust(WorkspaceCheckpoint {
                        machine,
                        id: id.clone(),
                        prompt: MessageId::new(prompt.as_str()),
                        assistant,
                        tool_use,
                        snapshot,
                    }))
                }
                // 修复线 F12：Node 写的没有 `rustCli`，以前一律不给出（不能回退、没有改动摘要）；现在记下 URI
                // 里的会话与 artifact，冷读取再按 Node 的产物文件补出引用。
                (Some(assistant), None, None) => parse_uri(&uri).map(|(session, artifact)| {
                    Active::Node(NodeCheckpoint {
                        id: id.clone(),
                        prompt: MessageId::new(prompt.as_str()),
                        assistant,
                        session: SessionId::new(session),
                        artifact: ArtifactId::new(artifact),
                    })
                }),
                _ => None,
            };
            Some(Stored {
                id,
                prompt,
                uri,
                sequence: data["sequenceNumber"].as_u64().unwrap_or(0),
                kernel,
            })
        })
        .collect();
    // Node `restoreWorkspaceCheckpointEntries`：按 sequenceNumber，再按创建时间（list 已按时间）。
    out.sort_by_key(|stored| stored.sequence);
    Ok(out)
}

/// Node `parseArtifactUri`：`zcode-artifact://<encodeURIComponent(会话)>/<encodeURIComponent(artifact)>`。
fn parse_uri(uri: &str) -> Option<(String, String)> {
    let rest = uri.strip_prefix(rust_cli_kernel_api::persisted_output::ARTIFACT_SCHEME)?;
    let (session, artifact) = rest.split_once('/')?;
    let (session, artifact) = (decode(session)?, decode(artifact.trim_start_matches('/'))?);
    (!session.is_empty() && !artifact.is_empty()).then_some((session, artifact))
}

/// JS `decodeURIComponent`；格式不对时为空（Node 抛错，这个 checkpoint 不给出）。
fn decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = value.get(index + 1..index + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// 冷读取：没有被文件回退撤销的回合里的 checkpoint（两边写的），按 `sequenceNumber`；本机的在前，再是远端机器上的
/// （R3：各自按自己的回退记录排除）。
pub fn active_checkpoints(
    conn: &Connection,
    session: &SessionId,
) -> Result<Vec<Active>, StoreError> {
    let mut out = Vec::new();
    for kind in [Kind::Node, Kind::Remote] {
        let rewound: Vec<String> = entries::list(conn, session.as_str(), Some(kind.file_rewind()))
            .storage()?
            .into_iter()
            .filter(|entry| entry.data["payload"]["reason"] == "file_summary_rewind")
            .filter_map(|entry| {
                entry.data["payload"]["targetMessageId"]
                    .as_str()
                    .map(str::to_owned)
            })
            .collect();
        out.extend(
            stored(conn, session.as_str(), kind)?
                .into_iter()
                .filter(|stored| !rewound.contains(&stored.prompt))
                .filter_map(|stored| stored.kernel),
        );
    }
    Ok(out)
}

#[cfg(test)]
#[path = "rewind_tests.rs"]
mod tests;
