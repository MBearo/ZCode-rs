//! `MessageAppended` → Node message / part 记录（docs/specs/rust-cli/node-store.md §4.1、§4.2）。
//!
//! 记录模板与 Node 逐字节一致（`records`）；Node 记录里内核消息没有的字段（父消息、轮次、
//! provider、模式、目录）从库里读（`state`）。
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, StopReason, SystemMessage, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::store::{Fact, StoreError};
use serde_json::{Value, json};

use crate::facts::{Commit, unsupported};
use crate::ids::{created_at, part_id, turn_id};
use crate::records::{self, Assistant, UserPrompt};
use crate::repo::{messages, sessions};
use crate::state;
use crate::{OrStorage, storage};
use crate::{notices, tool_parts};

pub fn append(ctx: &Commit<'_>, message: &Message) -> Result<(), StoreError> {
    let id = message.id().as_str();
    // message.id 是全库主键：同一个 id 再次追加说明内核重复提交，不能悄悄覆盖或叠加 part。
    if messages::exists(ctx.conn, id).storage()? {
        return Err(storage(format!("message {id} is already stored")));
    }
    match message {
        Message::User(user) => match &user.origin {
            UserOrigin::Human if notices::is_interrupt(user) => {
                notices::append_interrupt(ctx, user)
            }
            UserOrigin::Human if notices::is_bash_output(user) => {
                notices::append_bash_output(ctx, user)
            }
            UserOrigin::Human => append_user(ctx, user),
            UserOrigin::ToolResult { assistant } => tool_parts::settle(ctx, user, assistant),
            UserOrigin::Meta { source } => notices::append_meta(ctx, user, source),
            UserOrigin::CompactSummary => crate::compaction::append_summary(ctx, user),
        },
        Message::Assistant(assistant) => append_assistant(ctx, assistant),
        Message::Attachment(attachment) => notices::append_attachment(ctx, attachment),
        Message::System(SystemMessage::CompactBoundary(boundary)) => {
            crate::compaction::append_boundary(ctx, boundary)
        }
        Message::System(SystemMessage::Interrupted { .. }) => Err(unsupported("system message")),
    }
}

pub(crate) fn block_kind(block: &ContentBlock) -> &'static str {
    match block {
        ContentBlock::Text { .. } => "text",
        ContentBlock::Image { .. } => "image",
        ContentBlock::Thinking { .. } => "thinking",
        ContentBlock::RedactedThinking { .. } => "redacted_thinking",
        ContentBlock::ToolUse { .. } => "tool_use",
        ContentBlock::ToolResult { .. } => "tool_result",
        // 不落库（同 Node）：WebSearch 的旁路请求里才用；主回复里端点自己产生的，query 组装时已去掉（工作线 F90，
        // docs/specs/rust-cli/query.md §1.9）。这里仍拒收，挡住没走 query 的写入。
        ContentBlock::ServerToolUse { .. } => "server_tool_use",
        ContentBlock::WebSearchToolResult { .. } => "web_search_tool_result",
    }
}

/// Node `persistUserPrompt`：user 消息与每个文本块一个 text part。会话还没有标题时按 Node
/// `titleFromInput` 取首条输入作标题（Node 在首条输入时建会话行）。
fn append_user(ctx: &Commit<'_>, user: &UserMessage) -> Result<(), StoreError> {
    let mut texts = Vec::with_capacity(user.content.len());
    // 粘贴的图片（tui-input.md §3.2）：文字 part 之后的 Node `file` part（`persistUserPrompt` 的附件顺序）。
    let mut images = Vec::new();
    for block in &user.content {
        match block {
            ContentBlock::Text { text } => texts.push(text.as_str()),
            ContentBlock::Image { source } => images.push(source),
            other => return Err(unsupported(&format!("user {} block", block_kind(other)))),
        }
    }
    let id = user.id.as_str();
    let created = created_at(id, ctx.now);
    // 批次的回合锚点指向这条输入时用锚点的轮次（会话 actor 生成，node-store.md §4，修复线 F6）。
    let turn = ctx
        .anchored_turn(id)
        .unwrap_or_else(|| turn_id(&(ctx.uuid)()));
    let info = records::user_message(&UserPrompt {
        id,
        session: ctx.session,
        created,
        selection: state::selection(ctx.conn, ctx.session).storage()?,
        turn: &turn,
        command: ctx.command,
    });
    messages::save_message(ctx.conn, &info, ctx.now as i64).storage()?;
    ctx.opened(id, &turn);
    for text in &texts {
        // Node `persistUserPrompt(displayInput, …, {modelInput})`：text 是界面原文，模型文本在
        // `metadata.modelInput`（冷读取优先读它）。修复线 F2：bash 模式与 skill 命令以前把 `<bash-input>`、
        // `<command-message>` 标签当用户原文存，App 显示成带标签的用户气泡。
        let shown = display_input(text);
        let model = (shown != *text).then_some(*text);
        let part = records::user_text_part(
            part_id(ctx.now, &(ctx.uuid)()),
            (ctx.session, id),
            (&shown, model),
            (created, created),
        );
        messages::save_part(ctx.conn, &part, ctx.now as i64).storage()?;
    }
    for source in images {
        let part = crate::tool_content::file_part(
            part_id(ctx.now, &(ctx.uuid)()),
            (ctx.session, id),
            source,
        );
        messages::save_part(ctx.conn, &part, ctx.now as i64).storage()?;
    }
    let title = sessions::Update {
        id: ctx.session.to_owned(),
        // 用户调用 skill 的输入是命令标签：临时标题按用户输入的样子（`/<名字> <参数>`，ext.md §2.7）。
        title: Some(records::title_from_input(&display_input(&texts.join("\n")))),
        expected_title_sources: vec!["default".to_owned()],
        title_source: Some("first_input".to_owned()),
        ..Default::default()
    };
    sessions::update(ctx.conn, &title, ctx.now as i64).storage()?;
    Ok(())
}

/// Node 流式请求没有收到停止原因时的 `finishReason`。
const UNKNOWN_FINISH: &str = "unknown";

/// Anthropic `stop_reason` → AI SDK 的 `finishReason`（Node 存的值）。
pub fn finish_reason(reason: StopReason) -> &'static str {
    match reason {
        StopReason::EndTurn | StopReason::StopSequence | StopReason::PauseTurn => "stop",
        // 窗口用满照 AI SDK 的 `mapAnthropicStopReason` 写 `length`（provider-errors.md §2.5，F113）。
        StopReason::MaxTokens | StopReason::ModelContextWindowExceeded => "length",
        StopReason::ToolUse => "tool-calls",
        StopReason::Refusal => "content-filter",
        StopReason::Unknown => "other",
    }
}

/// Anthropic `usage` → Node 归一化用量（AI SDK：输入含缓存读写，`total` = 输入 + 输出）→
/// `toTokenUsageInfo`。没有用量时是 Node `emptyTokenUsageInfo`。
fn tokens(usage: Option<&rust_cli_kernel_api::message::Usage>) -> Value {
    let Some(usage) = usage else {
        return records::tokens(None);
    };
    let input = usage
        .input_tokens
        .saturating_add(usage.cache_creation_input_tokens)
        .saturating_add(usage.cache_read_input_tokens);
    records::tokens(Some(&json!({
        "inputTokens": input,
        "outputTokens": usage.output_tokens,
        "totalTokens": input.saturating_add(usage.output_tokens),
        "cacheReadTokens": usage.cache_read_input_tokens,
        "cacheWriteTokens": usage.cache_creation_input_tokens,
    })))
}

/// 一个模型步骤的 assistant（Node `turn-model-step` → `turn-stop` 的最终状态）：已完成的消息，
/// 以及 `step-start`、每个推理块一个 reasoning part、一个 text part（全部文本块拼接）、每个 tool_use
/// 一个 pending 的 tool part（§4.6）、`step-finish`。
fn append_assistant(ctx: &Commit<'_>, assistant: &AssistantMessage) -> Result<(), StoreError> {
    for block in &assistant.content {
        match block {
            ContentBlock::Text { .. }
            | ContentBlock::Thinking { .. }
            | ContentBlock::RedactedThinking { .. }
            | ContentBlock::ToolUse { .. } => {}
            other => {
                return Err(unsupported(&format!(
                    "assistant {} block",
                    block_kind(other)
                )));
            }
        }
    }
    let row = sessions::get(ctx.conn, ctx.session)
        .storage()?
        .ok_or_else(|| storage(format!("session {} disappeared", ctx.session)))?;
    let (parent, turn) = ctx
        .current_user()?
        .ok_or_else(|| storage("an assistant message needs a stored user message first"))?;
    let selection = state::selection(ctx.conn, ctx.session).storage()?;
    // Node `persistAssistantMessage(…, model)`：modelId / providerId 是这次请求用的目录模型（同批的
    // `ModelCalled`），没有请求时是会话当前的选择。修复线 F2：以前 modelId 取响应里的模型名（可能为空、带日期或是
    // 网关别名），App 的稳定 fork 会按它选中目录外的模型。
    let requested = ctx.batch.iter().find_map(|fact| match fact {
        Fact::ModelCalled { call } if call.message == assistant.id => Some(&call.selection),
        _ => None,
    });
    let current = |key: &str| selection.as_ref().and_then(|s| s[key].as_str());
    let provider = requested
        .map(|s| s.provider.as_str())
        .or_else(|| current("providerId"))
        .unwrap_or("");
    let model = requested
        .map(|s| s.model.as_str())
        .or_else(|| current("modelId"))
        .unwrap_or(&assistant.model);
    let execution = state::execution(ctx.conn, &row)
        .storage()?
        .unwrap_or_default();
    let id = assistant.id.as_str();
    let created = created_at(id, ctx.now);
    let tokens = tokens(assistant.usage.as_ref());
    // Node 总写 `finish` 与 step-finish 的 `reason`（App 协议的 step-finish `reason` 是必填的 string）。流式中被
    // 中断时没有停止原因，按 Node 流式请求的缺省值 `unknown` 写（model.ts 的 `finishReason`），读回 Unknown。
    // 修复线 F2：以前不写这两个键，App 的 strict schema 拒收，打不开会话。
    let finish = assistant.stop_reason.map_or(UNKNOWN_FINISH, finish_reason);
    let info = records::assistant_message(&Assistant {
        id,
        session: ctx.session,
        parent: &parent,
        created,
        completed: ctx.now,
        provider,
        model,
        mode: &execution.mode,
        plan: execution.plan,
        directory: &row.directory,
        tokens: tokens.clone(),
        finish,
        turn: turn.as_deref(),
        // 续写占位（工作线 F77）：落库后 App 不显示它，Node 与本工程续写时照常发给模型（§4.2.1）。
        model_only: assistant.is_resume_sentinel(),
    });
    messages::save_message(ctx.conn, &info, ctx.now as i64).storage()?;
    let owner = (ctx.session, id);
    let span = (created, ctx.now);
    let mut parts = vec![records::step_start_part(
        part_id(ctx.now, &(ctx.uuid)()),
        owner,
    )];
    for block in &assistant.content {
        let (text, metadata) = match block {
            ContentBlock::Thinking {
                thinking,
                signature,
            } => (
                thinking.as_str(),
                signature
                    .as_ref()
                    .map(|signature| json!({"anthropic": {"signature": signature}})),
            ),
            ContentBlock::RedactedThinking { data } => {
                ("", Some(json!({"anthropic": {"redactedData": data}})))
            }
            _ => continue,
        };
        // Node `hasAssistantReasoningContent`：没有文本也没有 providerOptions 的推理块不写。
        if text.is_empty() && metadata.is_none() {
            continue;
        }
        let part_id = part_id(ctx.now, &(ctx.uuid)());
        parts.push(records::reasoning_part(
            part_id, owner, text, metadata, span,
        ));
    }
    // Node 每个步骤只写一个 text part：AI SDK 的 `text` 是全部文本块直接拼接。
    let text: String = assistant
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    if !text.is_empty() {
        let part_id = part_id(ctx.now, &(ctx.uuid)());
        parts.push(records::text_part(part_id, owner, &text, span));
    }
    let calls = assistant.content.iter().filter_map(|block| match block {
        ContentBlock::ToolUse { id, name, input } => Some((id, name.as_str(), input)),
        _ => None,
    });
    for (index, call) in calls.enumerate() {
        let part_id = part_id(ctx.now, &(ctx.uuid)());
        parts.push(tool_parts::pending(part_id, owner, index, call));
    }
    parts.push(records::step_finish_part(
        part_id(ctx.now, &(ctx.uuid)()),
        owner,
        finish,
        tokens,
    ));
    for part in &parts {
        messages::save_part(ctx.conn, part, ctx.now as i64).storage()?;
    }
    Ok(())
}

/// 命令标签还原成用户输入（kernel-api `user_command::display`），其余原样。
fn display_input(text: &str) -> String {
    rust_cli_kernel_api::user_command::display(text)
        .or_else(|| rust_cli_kernel_api::user_input::display(text))
        .unwrap_or_else(|| text.to_owned())
}
