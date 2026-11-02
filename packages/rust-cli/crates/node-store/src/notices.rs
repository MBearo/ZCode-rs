//! 合成 user 消息：attachment、meta 消息、用户中断与 bash 模式的输出（docs/specs/rust-cli/node-store.md §4.5）。
//!
//! 记录形状取自 Node `persistRuntimeReminderMessage`（`runtime-reminder-persistence.ts`）：
//! model-only 的 synthetic user 消息，每段文本一个 synthetic text part，`metadata.source` 是来源。
//! 本工程在 part 的 `metadata.rustCli` 里另记消息种类（attachment 还有 `data`），冷加载按它还原。
//! Node 读取时只看来源与文本：文本已带 `<system-reminder>` 包装、或来源不在它的提醒表里时，
//! 原文作为 user 文本发给模型。attachment 的内容块（`data.blocks`，图片等）写成 text part 之后的
//! text / file part（docs/specs/rust-cli/node-store.md §4.5）。
use rust_cli_kernel_api::message::{
    ATTACHMENT_BLOCKS, AttachmentMessage, ContentBlock, UserMessage,
};
use rust_cli_kernel_api::store::StoreError;
use rust_cli_node_records::notices::{
    ATTACHMENT, BASH_OUTPUT, BASH_OUTPUT_SOURCE, INTERRUPT, INTERRUPT_SOURCE, META, RENAMED_SOURCES,
};
use rust_cli_node_records::records::RUST_CLI;
use serde_json::{Value, json};

use crate::OrStorage;
use crate::facts::{Commit, unsupported};
use crate::ids::{created_at, part_id};
use crate::keys;
use crate::records::{self, AGENT};
use crate::repo::messages;
use crate::state;

/// 内核的用户中断消息。它的来源记为 Human，但不是用户输入：Node 不为中断写 user 消息（只把会话
/// 标成中断），存成真实输入会被 App 显示成用户气泡、改写标题，并成为下一条 assistant 的父消息。
///
/// 判定在 kernel-api（`user_input::is_interrupt`）：会话 actor 按同一个判定换当前轮（§4，修复线 F6）。
pub fn is_interrupt(user: &UserMessage) -> bool {
    rust_cli_kernel_api::user_input::is_interrupt(user)
}

/// bash 模式的输出消息（`<bash-stdout>` / `<bash-stderr>`，kernel-api `user_input::bash_output`）。来源记为 Human，
/// 但不是用户输入：Node 没有 bash 模式，按 Node 运行时合成消息存，不开新的真实输入轮次（修复线 F2，以前存成
/// real_user，App 显示成带标签的用户气泡，并多出一轮）；冷读取还原成原来的 Human 消息，模型文本不变。
pub fn is_bash_output(user: &UserMessage) -> bool {
    rust_cli_kernel_api::user_input::is_bash_output(user)
}

pub fn append_attachment(
    ctx: &Commit<'_>,
    attachment: &AttachmentMessage,
) -> Result<(), StoreError> {
    let mut marker = json!({"message": ATTACHMENT});
    if !attachment.data.is_null() {
        let mut data = attachment.data.clone();
        // 内容块另存成 part，`rustCli.data` 不重复存图片。
        if let Some(fields) = data.as_object_mut() {
            fields.remove(ATTACHMENT_BLOCKS);
        }
        marker["data"] = data;
    }
    let mut blocks = vec![ContentBlock::Text {
        text: attachment.text.clone(),
    }];
    blocks.extend(attachment.blocks());
    let source = if RENAMED_SOURCES.contains(&attachment.source.as_str()) {
        keys::named(&attachment.source)
    } else {
        attachment.source.clone()
    };
    save(ctx, attachment.id.as_str(), &source, &blocks, marker)
}

pub fn append_meta(ctx: &Commit<'_>, user: &UserMessage, source: &str) -> Result<(), StoreError> {
    let texts = texts(user, META)?;
    save(
        ctx,
        user.id.as_str(),
        source,
        &texts,
        json!({"message": META}),
    )
}

pub fn append_interrupt(ctx: &Commit<'_>, user: &UserMessage) -> Result<(), StoreError> {
    let texts = texts(user, INTERRUPT)?;
    let marker = json!({"message": INTERRUPT});
    save(ctx, user.id.as_str(), INTERRUPT_SOURCE, &texts, marker)
}

pub fn append_bash_output(ctx: &Commit<'_>, user: &UserMessage) -> Result<(), StoreError> {
    let texts = texts(user, BASH_OUTPUT)?;
    let marker = json!({"message": BASH_OUTPUT});
    save(
        ctx,
        user.id.as_str(),
        &keys::named(BASH_OUTPUT_SOURCE),
        &texts,
        marker,
    )
}

fn texts(user: &UserMessage, what: &str) -> Result<Vec<ContentBlock>, StoreError> {
    user.content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { .. } => Ok(block.clone()),
            _ => Err(unsupported(&format!(
                "{what} message with a non-text block"
            ))),
        })
        .collect()
}

/// Node `reminderPartMetadata` 去掉 `runtimeMessage`（那是 Node 运行时的来源名，这里的来源是
/// 内核的 attachment type）。
fn metadata(source: &str) -> Value {
    json!({"source": source, "visibility": "model-only"})
}

/// Node `persistRuntimeReminderMessage` 的消息。`modelSelection` 只带 provider 与 model（Node
/// `resolvePersistedReminderModel`）；没有工具目录时 `tools` 为 `{}`；`system` 未设置时省略。
fn message(
    (id, session): (&str, &str),
    created: u64,
    source: &str,
    selection: Option<&Value>,
    turn: Option<&str>,
) -> Value {
    let mut out = json!({"id": id, "sessionID": session, "role": "user",
        "time": {"created": created}, "agent": AGENT, "metadata": metadata(source)});
    if let Some(selection) = selection {
        out["modelSelection"] =
            json!({"providerId": selection["providerId"], "modelId": selection["modelId"]});
    }
    if let Some(anchor) = records::anchor(turn, Some("synthetic"), None) {
        out["anchor"] = anchor;
    }
    out["semantics"] = json!({"origin": "agent_runtime", "kind": "system_reminder",
        "source": source, "uiVisibility": "hidden", "providerVisibility": "visible",
        "transcriptVisibility": "hidden"});
    out["synthetic"] = true.into();
    out["tools"] = json!({});
    out["visibility"] = "model-only".into();
    out
}

/// 一条合成消息：每个文字块一个 synthetic text part（第一个带 `rustCli` 标记的 metadata），图片是 Node `file` part。
fn save(
    ctx: &Commit<'_>,
    id: &str,
    source: &str,
    blocks: &[ContentBlock],
    marker: Value,
) -> Result<(), StoreError> {
    let created = created_at(id, ctx.now);
    let selection = state::selection(ctx.conn, ctx.session).storage()?;
    // 锚定当前轮（Node `buildProjectionAnchor(turnTraceContext, "synthetic")`）。
    let turn = ctx.current_user()?.and_then(|(_, turn)| turn);
    let info = message(
        (id, ctx.session),
        created,
        source,
        selection.as_ref(),
        turn.as_deref(),
    );
    messages::save_message(ctx.conn, &info, ctx.now as i64).storage()?;
    let mut metadata = metadata(source);
    metadata[RUST_CLI] = marker;
    for block in blocks {
        let part = match block {
            ContentBlock::Text { text } => {
                json!({"id": part_id(ctx.now, &(ctx.uuid)()), "sessionID": ctx.session,
                "messageID": id, "type": "text", "text": text, "synthetic": true,
                "time": {"start": created, "end": created}, "metadata": metadata})
            }
            ContentBlock::Image { source } => crate::tool_content::file_part(
                part_id(ctx.now, &(ctx.uuid)()),
                (ctx.session, id),
                source,
            ),
            other => {
                return Err(unsupported(&format!(
                    "synthetic message with a {} block",
                    crate::append::block_kind(other)
                )));
            }
        };
        messages::save_part(ctx.conn, &part, ctx.now as i64).storage()?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "notices_tests.rs"]
mod tests;
