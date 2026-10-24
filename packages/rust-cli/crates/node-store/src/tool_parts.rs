//! 工具调用与结果 ↔ Node tool part（docs/specs/rust-cli/node-store.md §4.6）。
//!
//! 写入：assistant 的 tool_use 块写成 pending 的 tool part（Node `persistPendingToolPart`），`metadata.rustCli` 带本工程
//! 的标记；工具结果（单独的 user 消息，`origin = ToolResult`）到达时把同一个 part 更新成 completed / error（Node
//! `settleToolResults`），不新建 user 消息行。结果消息的 id、提交顺序与位置键（结算时最后一行）记在 part 的
//! `metadata.rustCli`（换掉 pending 时的标记）。
//!
//! 读取：按 Node `selectToolPartsForHistory` 选 part，拆回内核的消息形状：assistant 里的 tool_use 块，
//! 加上每个结果消息一条 `origin = ToolResult` 的 user 消息，按提交顺序；位置由 hydrate 按位置键放。没有标记的（Node 写的）
//! 未结调用在内存里补 Node 的中断结果，不写库（[`results`]，工作线 F86、F88）。
use rusqlite::OptionalExtension;
use rust_cli_base::js::json::stringify;
use rust_cli_base::js::string::trim;
use rust_cli_kernel_api::ids::{MessageId, ToolUseId};
use rust_cli_kernel_api::message::{
    ContentBlock, Message, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::store::{Fact, StoreError};
use serde_json::{Value, json};

use crate::facts::{Commit, unsupported};
use crate::ids::{created_at, part_id};
use crate::notices::RUST_CLI;
use crate::repo::codecs::decode_part;
use crate::repo::messages;
use crate::{OrStorage, operations, state, storage, tool_content};

/// Node `projectToolNameForNonEmptyBoundary`：空名字换成占位名，原名记在 `providerToolName`。
const EMPTY_TOOL_NAME: &str = "empty_tool_name";
const PROVIDER_TOOL_NAME: &str = "providerToolName";

/// Node `ToolStateCompleted.metadata.schemaVersion`（`COMPLETED_TOOL_PART_METADATA_SCHEMA_VERSION`）。
const COMPLETED_SCHEMA_VERSION: u64 = 1;

/// Node `createErrorResult` 给模型的业务失败文本：`<tool_use_error>原文</tool_use_error>`。
const TOOL_USE_ERROR: (&str, &str) = ("<tool_use_error>", "</tool_use_error>");

/// Node 冷恢复给未结调用补的结果文字（`is_error`；Node `session-history-hydrator.ts` 的 `INTERRUPTED_TOOL_RESULT`）。
/// 冷读取给 Node 写的未结调用补它（[`results`]，工作线 F88）。
pub const NODE_INTERRUPTED_CALL: &str = "[Tool execution was interrupted before resume]";

fn projected(name: &str) -> (&str, Value) {
    if trim(name).is_empty() {
        (EMPTY_TOOL_NAME, json!({PROVIDER_TOOL_NAME: name}))
    } else {
        (name, json!({}))
    }
}

/// Node `toRecordInput`：tool part 的 input 必须是对象，其他值包成 `{value}`。
pub fn record_input(input: &Value) -> Value {
    if input.is_object() {
        input.clone()
    } else {
        json!({"value": input})
    }
}

/// assistant 消息里第 `index` 个 tool_use 块的 pending part（Node `persistPendingToolPart`）。
///
/// 工作线 F88（用户决定 2026-10-24）：`metadata.rustCli = {pending: true}` 是本工程的标记，冷读取按它认出未结调用是谁写的
/// （[`results`]）；结算时整个 `rustCli` 换成结果的标记（[`settle`]），所以本工程写的 tool part 总有 `rustCli`，Node 写的
/// 没有。以前 pending part 与 Node 写的形状相同，只能按 Node 的写入顺序（running、`step-finish`）去猜，Node 改写入顺序就
/// 认错。
pub fn pending(
    id: String,
    (session, message): (&str, &str),
    index: usize,
    (call, name, input): (&ToolUseId, &str, &Value),
) -> Value {
    let (tool, mut metadata) = projected(name);
    metadata[RUST_CLI] = json!({"pending": true});
    let raw = stringify(&json!({"tool": tool, "input": input}));
    json!({"id": id, "sessionID": session, "messageID": message, "type": "tool",
        "callID": call.as_str(), "declarationIndex": index, "tool": tool, "metadata": metadata,
        "state": {"status": "pending", "input": record_input(input), "raw": raw}})
}

/// 一个 assistant 消息里某个调用最后写的 tool part（Node 同一调用只恢复最后新建的记录）。
fn find_part(ctx: &Commit<'_>, assistant: &str, call: &str) -> Result<Option<Value>, StoreError> {
    let row: Option<(String, String)> = ctx
        .conn
        .prepare_cached(
            "select id, data from part where message_id = ? and session_id = ?
          and (case when json_valid(data) then json_extract(data, '$.type') end) = 'tool'
          and (case when json_valid(data) then json_extract(data, '$.callID') end) = ?
          order by sequence desc, rowid desc limit 1",
        )
        .storage()?
        .query_row([assistant, ctx.session, call], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()
        .storage()?;
    row.map(|(id, data)| decode_part(&data, &id, ctx.session, assistant).storage())
        .transpose()
}

/// 这个 assistant 里已经有结果的 tool part 数：下一个结果的提交序号。
fn settled(ctx: &Commit<'_>, assistant: &str) -> Result<u64, StoreError> {
    let count: i64 = ctx
        .conn
        .prepare_cached(
            "select count(*) from part where message_id = ? and session_id = ?
          and (case when json_valid(data) then json_extract(data, '$.type') end) = 'tool'
          and (case when json_valid(data) then json_extract(data, '$.state.status') end)
            in ('completed', 'error')",
        )
        .storage()?
        .query_row([assistant, ctx.session], |r| r.get(0))
        .storage()?;
    Ok(u64::try_from(count).unwrap_or(0))
}

/// 会话里最后一条消息行（冷读取的行序；`sequence` 在会话内唯一、递增）：结果的位置键（node-store.md §4.6）。
fn last_row(ctx: &Commit<'_>) -> Result<Option<String>, StoreError> {
    ctx.conn
        .prepare_cached(
            "select id from message where session_id = ? order by sequence desc limit 1",
        )
        .storage()?
        .query_row([ctx.session], |r| r.get(0))
        .optional()
        .storage()
}

/// 只有一个文本块、恰好是 `<tool_use_error>原文</tool_use_error>` 的失败结果：原文。
fn bare_error(content: &tool_content::Stored) -> Option<&str> {
    let (open, close) = TOOL_USE_ERROR;
    content
        .layout
        .is_none()
        .then_some(content.output.as_str())?
        .strip_prefix(open)?
        .strip_suffix(close)
}

/// Node `settleToolResults` 的 completed / error 状态。`time.start` 是执行器开工的时刻（同批 `ToolCalled`，
/// 没有时是 assistant 的创建时间），`time.end` 是结果消息的创建时间。
fn settled_state(
    part: &Value,
    content: &tool_content::Stored,
    is_error: bool,
    (start, end): (u64, u64),
) -> Value {
    let input = part["state"]["input"].clone();
    let mut state = if is_error {
        let mut metadata = json!({"modelContent": content.output});
        if let Some(layout) = &content.layout {
            metadata["modelContentLayout"] = layout.clone();
        }
        // 修复线 F12：Node `createErrorResult` 的 `state.error` 是原文（App 工具卡显示它），带标签的模型文本只在
        // `metadata.modelContent`（冷读取读它）。以前 `state.error` 也存带标签的文本，App 显示出标签。
        let error = bare_error(content).unwrap_or(&content.output);
        json!({"status": "error", "input": input, "error": error, "metadata": metadata,
            "time": {"start": start, "end": end}})
    } else {
        let mut metadata = json!({"schemaVersion": COMPLETED_SCHEMA_VERSION});
        if let Some(layout) = &content.layout {
            metadata["modelContentLayout"] = layout.clone();
        }
        json!({"status": "completed", "input": input, "output": content.output,
            "title": part["tool"], "metadata": metadata, "time": {"start": start, "end": end}})
    };
    if !content.attachments.is_empty() {
        state["attachments"] = Value::Array(content.attachments.clone());
    }
    state
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
    let after = last_row(ctx)?;
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
        let order = settled(ctx, owner)?;
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
        let status = if is_error { "error" } else { "completed" };
        let input = &part["state"]["input"];
        operations::settle(ctx, (provider_name(&part), input), call.as_str(), status)?;
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
    let row: Option<(String, String, String)> = ctx
        .conn
        .prepare_cached(
            "select id, message_id, data from part where session_id = ?
          and (case when json_valid(data) then json_extract(data, '$.type') end) = 'tool'
          and (case when json_valid(data) then json_extract(data, '$.callID') end) = ?
          order by sequence desc, rowid desc limit 1",
        )
        .storage()?
        .query_row([ctx.session, call.as_str()], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .optional()
        .storage()?;
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

/// Node `selectToolPartsForHistory`：同一调用只取最后的记录；都有声明序号时按序号排。
pub fn selected(parts: &[Value]) -> Vec<&Value> {
    let tools: Vec<&Value> = parts.iter().filter(|part| part["type"] == "tool").collect();
    let call = |part: &Value| part["callID"].as_str().unwrap_or("").to_owned();
    let mut latest: Vec<&Value> = tools
        .iter()
        .enumerate()
        .filter(|(index, part)| {
            !tools[index + 1..]
                .iter()
                .any(|later| call(later) == call(part))
        })
        .map(|(_, part)| *part)
        .collect();
    if latest.iter().all(|part| part["declarationIndex"].is_u64()) {
        latest.sort_by_key(|part| part["declarationIndex"].as_u64());
    }
    latest
}

/// 模型给的工具名（Node `historyToolNameFromPart`：有 `providerToolName` 时取它）。
fn provider_name(part: &Value) -> &str {
    part["metadata"][PROVIDER_TOOL_NAME]
        .as_str()
        .or_else(|| part["tool"].as_str())
        .unwrap_or("")
}

/// tool part → tool_use 块。
pub fn tool_use(part: &Value) -> ContentBlock {
    ContentBlock::ToolUse {
        id: ToolUseId::new(part["callID"].as_str().unwrap_or("")),
        name: provider_name(part).to_owned(),
        input: part["state"]["input"].clone(),
    }
}

/// 本工程写的 tool part：有 `metadata.rustCli`（pending 时的标记或结果的标记，[`pending`]）。
fn ours(part: &Value) -> bool {
    part["metadata"][RUST_CLI].is_object()
}

/// 一个调用的结果：(提交序号, 消息 id, 位置键, 块)；本工程写的未结调用没有。
///
/// 已结的：提交序号与消息 id 取 `metadata.rustCli`（Node 写的没有：序号记作最大，消息 id 用 part id）。
/// 未结的（node-store.md §4.6，session-lifecycle.md §6.3.2）：有标记的是本工程写的，没有结果，由会话续写时补并落库
/// （§6.3）。没有标记的是 Node 写的（含工作线 F88 之前本工程写的旧数据），照 Node 冷恢复补一条 [`NODE_INTERRUPTED_CALL`]
/// （`is_error`，消息 id 用 part id，没有位置键，序号同 Node 写的结果），只在内存里，不写库（用户决定 2026-10-24：Node 的
/// 记录保持原样）。每次冷读取都在同一位置补同一条；会话把它当作已有结果，不再补，也不提交。
/// 修 bug（工作线 F88）：F86 按 running / `step-finish` 判断谁写的，补本工程的文字、排在已有结果之后；Node 补的是自己的
/// 文字、按声明顺序夹在已有结果中间，模型在两边续写时看到的不同。
fn result(part: &Value) -> Option<(u64, String, Option<String>, ContentBlock)> {
    let tool_use_id = ToolUseId::new(part["callID"].as_str().unwrap_or(""));
    let part_id = part["id"].as_str().unwrap_or("").to_owned();
    match part["state"]["status"].as_str() {
        Some("completed" | "error") => {
            let marker = &part["metadata"][RUST_CLI];
            let id = marker["resultMessageId"]
                .as_str()
                .map_or(part_id, str::to_owned);
            let block = ContentBlock::ToolResult {
                tool_use_id,
                content: tool_content::restore(&part["state"]),
                is_error: part["state"]["status"] == "error",
            };
            Some((
                marker["resultIndex"].as_u64().unwrap_or(u64::MAX),
                id,
                marker["afterMessageId"].as_str().map(str::to_owned),
                block,
            ))
        }
        _ if ours(part) => None,
        _ => {
            let block = ContentBlock::ToolResult {
                tool_use_id,
                content: vec![ToolResultContent::Text {
                    text: NODE_INTERRUPTED_CALL.to_owned(),
                }],
                is_error: true,
            };
            Some((u64::MAX, part_id, None, block))
        }
    }
}

/// 一条 assistant 的结果消息，各带位置键（结算时最后一行；没有时为 None）。`tools` 是声明顺序（[`selected`]），按
/// 提交序号稳定排序：本工程写的结果按提交顺序在前，Node 写的结果与补的中断结果按声明顺序在后（同 Node 冷恢复）。同一条
/// 结果消息的多个块合成一条。
pub fn results(assistant: &MessageId, tools: &[&Value]) -> Vec<(Option<String>, Message)> {
    let mut done: Vec<(u64, String, Option<String>, ContentBlock)> =
        tools.iter().filter_map(|part| result(part)).collect();
    done.sort_by_key(|(order, _, _, _)| *order);
    let mut out: Vec<(Option<String>, UserMessage)> = Vec::new();
    for (_, id, after, block) in done {
        match out.last_mut() {
            Some((_, last)) if last.id.as_str() == id => last.content.push(block),
            _ => out.push((
                after,
                UserMessage {
                    id: MessageId::new(id),
                    content: vec![block],
                    origin: UserOrigin::ToolResult {
                        assistant: assistant.clone(),
                    },
                },
            )),
        }
    }
    out.into_iter()
        .map(|(after, user)| (after, Message::User(user)))
        .collect()
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
