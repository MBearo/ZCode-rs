//! Node `/fork` 的新会话（docs/specs/rust-cli/session-lifecycle.md §6.5.2、node-store.md §4.8）：会话行、模型与
//! 模式、克隆的消息（`fork_clone.rs`）、目标状态、时间线与分叉提示在一个 `begin immediate` 事务里写完。
//!
//! 依据：Node `session-fork.ts`（`buildForkedSessionInput`、`createForkedSession`、`forkConversationFromMessage`）、
//! `timeline-persistence.ts`（`persistAssistantTimelinePartForSession`）、`message-persistence.ts`
//! （`persistSyntheticUserNoticeForSession`）与 `helpers/rewind.ts` 的分叉对话提示。
use std::borrow::Cow;

use rusqlite::{Connection, TransactionBehavior};
use rust_cli_kernel_api::store::{Fact, ForkReceipt, SessionFork, StoreError};
use serde_json::{Value, json};

use rust_cli_node_records::notices::FORK_NOTICE_SOURCE;

use crate::branch::{Branch, select_branch};
use crate::facts::{self, Commit};
use crate::ids::{NodeIds, message_id, part_id, slugify};
use crate::repo::{messages, sessions};
use crate::{OrStorage, records, state, storage};

/// fork 出的会话的 slug 最多这么多个字符。
const MAX_FORK_SLUG_LEN: usize = 120;
const CONTINUE: &str = "Continue from this fork. Do not assume messages after the fork point happened in this session.";

fn parent(conn: &Connection, id: &str) -> Result<sessions::ForkParent, StoreError> {
    sessions::fork_parent(conn, id)
        .storage()?
        .ok_or_else(|| StoreError::NotFound {
            session: rust_cli_kernel_api::ids::SessionId::new(id),
        })
}

/// JS `String.prototype.slice(0, n)`（slug 只有 ASCII）。
fn head(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

/// Node `sanitizeSystemReminderBody`：中和正文里嵌套的 `<system-reminder>` 标签。
fn sanitize(text: &str) -> String {
    text.replace("</system-reminder", "&lt;/system-reminder")
        .replace("<system-reminder", "&lt;system-reminder")
}

/// 分叉提示的正文（Node `formatConversationForkNoticeBody`）。
fn notice_text(fork: &SessionFork) -> String {
    let lines = [
        "This session was forked from a previous session message.".to_owned(),
        format!("parentSessionId: {}", fork.parent),
        format!("targetMessageId: {}", fork.through),
        "No workspace checkpoint was restored for this fork.".to_owned(),
        CONTINUE.to_owned(),
    ];
    sanitize(&lines.join("\n"))
}

/// `forkContext`（提示的消息与 part 的 metadata）与时间线共用的字段；分叉对话不恢复文件，`restoredFileCount`
/// 总是 0（Node 同样写 0）。
fn context(fork: &SessionFork) -> Value {
    json!({"kind": "session_fork", "parentSessionId": fork.parent.as_str(),
        "targetMessageId": fork.through.as_str(), "restoredFileCount": 0})
}

/// Node `persistAssistantTimelinePartForSession`：`session_fork` 分割线。
fn timeline(
    ctx: &Commit<'_>,
    fork: &SessionFork,
    directory: &str,
    anchor: Option<&str>,
) -> Result<(), StoreError> {
    let id = message_id(ctx.now, &(ctx.uuid)());
    let row = sessions::get(ctx.conn, ctx.session)
        .storage()?
        .ok_or_else(|| storage("the forked session disappeared"))?;
    let execution = state::execution(ctx.conn, &row)
        .storage()?
        .unwrap_or_default();
    let selection = facts::node_selection(&fork.selection);
    let now = ctx.now;
    let info = json!({"id": id, "sessionID": ctx.session, "role": "assistant",
        "time": {"created": now, "completed": now}, "parentID": anchor.unwrap_or(&id),
        "modelId": selection["modelId"], "providerId": selection["providerId"],
        "mode": execution.mode, "planEnabled": execution.plan, "agent": records::AGENT,
        "path": {"cwd": directory, "root": directory}, "cost": 0, "tokens": records::tokens(None),
        "finish": "completed",
        "semantics": {"origin": "system", "kind": "timeline_event", "uiVisibility": "visible",
            "providerVisibility": "hidden", "transcriptVisibility": "visible"}});
    messages::save_message(ctx.conn, &info, now as i64).storage()?;
    let name = format!("fork_{}_{}", fork.parent, fork.through);
    let mut part = json!({"timelineType": "session_fork", "display": "separator",
        "status": "completed"});
    if let Some(anchor) = anchor {
        part["anchorMessageId"] = anchor.into();
    }
    part["parentSessionId"] = fork.parent.as_str().into();
    part["targetMessageId"] = fork.through.as_str().into();
    part["restoredFileCount"] = 0.into();
    part["time"] = json!({"start": now, "end": now});
    part["id"] = format!("part_{name}_timeline").into();
    part["sessionID"] = ctx.session.into();
    part["messageID"] = id.into();
    part["type"] = "timeline".into();
    messages::save_part(ctx.conn, &part, now as i64).storage()
}

/// Node `persistSyntheticUserNoticeForSession(source = fork)`。
fn notice(ctx: &Commit<'_>, fork: &SessionFork) -> Result<(), StoreError> {
    let id = message_id(ctx.now, &(ctx.uuid)());
    let fork_context = context(fork);
    let info = json!({"id": id, "sessionID": ctx.session, "role": "user",
        "time": {"created": ctx.now}, "agent": records::AGENT,
        "metadata": {"forkContext": fork_context, "source": FORK_NOTICE_SOURCE, "visibility": "model-only"},
        "modelSelection": facts::node_selection(&fork.selection),
        "semantics": {"origin": "agent_runtime", "kind": "fork_notice", "source": FORK_NOTICE_SOURCE,
            "uiVisibility": "hidden", "providerVisibility": "visible", "transcriptVisibility": "hidden"},
        "anchor": {"origin": "synthetic"}, "source": FORK_NOTICE_SOURCE, "synthetic": true, "tools": {},
        "visibility": "model-only"});
    messages::save_message(ctx.conn, &info, ctx.now as i64).storage()?;
    let part = json!({"id": part_id(ctx.now, &(ctx.uuid)()), "sessionID": ctx.session,
        "messageID": id, "type": "text", "text": notice_text(fork), "synthetic": true,
        "time": {"start": ctx.now, "end": ctx.now},
        "metadata": {"forkContext": fork_context, "runtimeMessage": {"source": "rewind_notice"},
            "source": FORK_NOTICE_SOURCE, "visibility": "model-only"}});
    messages::save_part(ctx.conn, &part, ctx.now as i64).storage()
}

pub(crate) fn fork(
    conn: &mut Connection,
    fork: &SessionFork,
    ids: &NodeIds,
) -> Result<ForkReceipt, StoreError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .storage()?;
    let parent = parent(&tx, fork.parent.as_str())?;
    let created = i64::try_from(fork.created_at_ms).unwrap_or(i64::MAX);
    let title = if parent.title.is_empty() {
        rust_cli_kernel_api::title::UNTITLED
    } else {
        parent.title.as_str()
    };
    let input = sessions::Create {
        id: fork.child.as_str().to_owned(),
        project_id: parent.project_id.clone(),
        workspace_id: parent.workspace_id.clone(),
        parent_id: Some(fork.parent.as_str().to_owned()),
        trace_id: Some(
            parent
                .trace_id
                .clone()
                .unwrap_or_else(|| fork.parent.as_str().to_owned()),
        ),
        task_type: Some("fork".to_owned()),
        slug: head(
            &format!(
                "{}-fork-{}",
                slugify(&parent.slug),
                rust_cli_base::js::base36(fork.created_at_ms)
            ),
            MAX_FORK_SLUG_LEN,
        ),
        directory: parent.directory.clone(),
        path: parent.path.clone(),
        title: format!("Fork of {title}"),
        title_source: Some("generated".to_owned()),
        version: parent.version.clone(),
        permission: parent.permission.clone(),
        time_created: Some(created),
    };
    sessions::create(&tx, &input, created).storage()?;
    let uuid = || ids.uuid();
    let ctx = Commit::new(
        &tx,
        fork.child.as_str(),
        (ids.now_ms(), &uuid),
        None,
        &[],
        None,
    );
    facts::apply(
        &ctx,
        &Fact::ModelSelected {
            selection: fork.selection.clone(),
        },
    )?;
    let state = rust_cli_kernel_api::permission::ExecutionState::from_parts(fork.mode, fork.plan);
    facts::apply(&ctx, &Fact::ExecutionChanged { state })?;
    // fork 源分支：只应用 `session.revert`，不按压缩边界截断（Node `forkSourceMessagesForSession`）。
    let records = messages::messages(&tx, fork.parent.as_str()).storage()?;
    let branch = select_branch(
        records.iter().map(Cow::Borrowed).collect(),
        &Branch::from_revert(parent.revert.as_ref()),
    );
    let end = branch
        .iter()
        .position(|record| record.id() == fork.through.as_str())
        .ok_or_else(|| {
            storage(format!(
                "Checkpoint message not found in session store: {}",
                fork.through
            ))
        })?
        + 1;
    let copied: Vec<&messages::WithParts> = branch[..end].iter().map(AsRef::as_ref).collect();
    let map = crate::fork_clone::copy_messages(&ctx, &copied)?;
    crate::fork_clone::copy_goal(&ctx, fork.parent.as_str(), &map)?;
    let anchor = map.get(fork.through.as_str()).map(String::as_str);
    timeline(&ctx, fork, &parent.directory, anchor)?;
    notice(&ctx, fork)?;
    facts::next_sequence(&ctx)?;
    tx.commit().storage()?;
    Ok(ForkReceipt { copied: end })
}

#[cfg(test)]
#[path = "fork_tests.rs"]
mod tests;
