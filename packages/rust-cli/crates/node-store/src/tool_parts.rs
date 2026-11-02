//! 工具调用与结果 ↔ Node tool part（docs/specs/rust-cli/node-store.md §4.6）。
//!
//! 写入：assistant 的 tool_use 块写成 pending 的 tool part（Node `persistPendingToolPart`），`metadata.rustCli` 带本工程
//! 的标记；工具结果（单独的 user 消息，`origin = ToolResult`）到达时把同一个 part 更新成 completed / error（Node
//! `settleToolResults`），不新建 user 消息行。结果消息的 id、提交顺序与位置键（结算时最后一行）记在 part 的
//! `metadata.rustCli`（换掉 pending 时的标记）。
//!
//! 读取的纯函数（选 part、拆回内核的消息形状、补 Node 的中断结果）与 part 的形状在 node-records 的 `tool_parts`（F128）。
use rust_cli_kernel_api::ids::{MessageId, ToolUseId};
use rust_cli_kernel_api::message::{ContentBlock, UserMessage};
use rust_cli_kernel_api::store::{Fact, StoreError};
use rust_cli_node_records::codecs::decode_part;
use rust_cli_node_records::records::RUST_CLI;
use rust_cli_node_records::tool_parts::settled_state;
use serde_json::{Value, json};

pub use rust_cli_node_records::tool_parts::pending;

use crate::facts::{Commit, unsupported};
use crate::ids::{created_at, part_id};
use crate::repo::messages;
use crate::{OrStorage, state, storage, tool_content};

/// 一个 assistant 消息里某个调用最后写的 tool part（Node 同一调用只恢复最后新建的记录）。
fn find_part(ctx: &Commit<'_>, assistant: &str, call: &str) -> Result<Option<Value>, StoreError> {
    messages::last_tool_part(ctx.conn, (ctx.session, assistant), call)
        .storage()?
        .map(|(id, data)| decode_part(&data, &id, ctx.session, assistant).storage())
        .transpose()
}

/// 工具结果消息：更新 `assistant` 里对应的 tool part。每个块都必须是 tool_result。
pub fn settle(
    ctx: &Commit<'_>,
    user: &UserMessage,
    assistant: &MessageId,
) -> Result<(), StoreError> {
    let mut results = Vec::with_capacity(user.content.len());
    for block in &user.content {
        match block {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => results.push((tool_use_id, content, *is_error)),
            _ => {
                return Err(unsupported(
                    "tool_result message with a non-tool_result block",
                ));
            }
        }
    }
    if results.is_empty() {
        return Err(unsupported("tool_result message without results"));
    }
    let owner = assistant.as_str();
    let info = state::message(ctx.conn, ctx.session, owner)
        .storage()?
        .filter(|info| info["role"] == "assistant")
        .ok_or_else(|| storage(format!("tool results need the stored assistant {owner}")))?;
    let created = info["time"]["created"].as_u64().unwrap_or(ctx.now);
    // 修 bug（工作线 F81 R2）：结果没有消息行，以前冷读取把结果都排在 assistant 之后，结算之间写下的行（hook 的附加
    // 上下文等）都排到结果之后，与实时的顺序不同。记下结算时最后一行，冷读取把结果插回它之后。
    let after = messages::last_message_id(ctx.conn, ctx.session).storage()?;
    for (call, content, is_error) in results {
        // 修复线 F12：Node 的 `time.start` 是 `result.startedAt`（执行器开工的时刻），同批的 `ToolCalled` 带它
        // （usage.md §2.3）。以前一律取 assistant 的创建时间（当时内核没有开工事实）。
        let start = started_at(ctx, call).unwrap_or(created);
        let end = created_at(user.id.as_str(), ctx.now).max(start);
        let mut part = find_part(ctx, owner, call.as_str())?
            .ok_or_else(|| storage(format!("{owner} has no tool part for {call}")))?;
        if !matches!(
            part["state"]["status"].as_str(),
            Some("pending" | "running")
        ) {
            return Err(storage(format!("tool_use {call} already has a result")));
        }
        let order = messages::settled_tool_parts(ctx.conn, (ctx.session, owner)).storage()?;
        let stored = tool_content::store(content, (ctx.session, owner), || {
            part_id(ctx.now, &(ctx.uuid)())
        });
        part["state"] = settled_state(&part, &stored, is_error, (start, end));
        if !part["metadata"].is_object() {
            part["metadata"] = json!({});
        }
        let mut marker = json!({"resultMessageId": user.id.as_str(), "resultIndex": order});
        if let Some(after) = &after {
            marker["afterMessageId"] = json!(after);
        }
        part["metadata"][RUST_CLI] = marker;
        messages::save_part(ctx.conn, &part, ctx.now as i64).storage()?;
    }
    Ok(())
}

/// 同批 `ToolCalled` 里这个调用的开工时刻。
fn started_at(ctx: &Commit<'_>, call: &ToolUseId) -> Option<u64> {
    ctx.batch.iter().find_map(|fact| match fact {
        Fact::ToolCalled { call: metered } if metered.tool_use == *call => {
            Some(metered.started_at_ms)
        }
        _ => None,
    })
}

/// 子代理调用关联到子会话（`Fact::SubagentLinked`，修复线 F2）：本会话里这个调用最后写的 tool part，在
/// `state.metadata` 里加 Node 子代理查询（`subagent-session-query.ts` 的 `candidateFromToolPart`）读的
/// `childSessionId`、`agentId`、`agentType`。与工具结果同批提交、排在结果之后，所以 part 已经有结果。以前一次性
/// 子代理（Explore / Plan）的结果没有 agentId 尾注，tool part 也没有这三个键，App 的子代理面板找不到子会话。
pub fn link(
    ctx: &Commit<'_>,
    call: &ToolUseId,
    (agent, child, agent_type): (&str, &str, &str),
) -> Result<(), StoreError> {
    let row =
        messages::last_tool_part_in_session(ctx.conn, ctx.session, call.as_str()).storage()?;
    let (id, message, data) = row.ok_or_else(|| storage(format!("no tool part for {call}")))?;
    let mut part = decode_part(&data, &id, ctx.session, &message).storage()?;
    if !matches!(
        part["state"]["status"].as_str(),
        Some("completed" | "error")
    ) {
        return Err(storage(format!("tool_use {call} has no result to link")));
    }
    let metadata = &mut part["state"]["metadata"];
    metadata["childSessionId"] = child.into();
    metadata["agentId"] = agent.into();
    metadata["agentType"] = agent_type.into();
    messages::save_part(ctx.conn, &part, ctx.now as i64).storage()
}

#[cfg(test)]
#[path = "tool_parts_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tool_order_tests.rs"]
mod order_tests;

#[cfg(test)]
#[path = "node_open_tests.rs"]
mod node_open_tests;
