//! token 估算：消息的粗估，与带用量的响应加之后消息的粗估。

use rust_cli_kernel_api::message::{
    ContentBlock, ImageSource, Message, SystemMessage, ToolResultContent, Usage,
};

/// 图片与文档的估算值。
pub const IMAGE_TOKENS: u64 = 2_000;

/// 字符数 / 4，四舍五入；字符数按 UTF-16 码元（JS 的 `length`）。唯一定义在 kernel-api（压缩后带回的 skill 也用它）。
pub(crate) fn rough(text: &str) -> u64 {
    rust_cli_kernel_api::compaction::rough_tokens(text)
}

fn result_content(content: &[ToolResultContent]) -> u64 {
    content
        .iter()
        .map(|item| match item {
            ToolResultContent::Text { text } => rough(text),
            ToolResultContent::Image { .. } | ToolResultContent::Document { .. } => IMAGE_TOKENS,
            ToolResultContent::ToolReference { tool_name } => rough(tool_name),
        })
        .sum()
}

fn block(block: &ContentBlock) -> u64 {
    match block {
        ContentBlock::Text { text } => rough(text),
        ContentBlock::Image {
            source: ImageSource::Base64 { .. } | ImageSource::Artifact { .. },
        } => IMAGE_TOKENS,
        ContentBlock::Thinking { thinking, .. } => rough(thinking),
        ContentBlock::RedactedThinking { data } => rough(data),
        ContentBlock::ToolUse { name, input, .. } => rough(&format!("{name}{input}")),
        ContentBlock::ToolResult { content, .. } => result_content(content),
        ContentBlock::ServerToolUse { name, input, .. } => rough(&format!("{name}{input}")),
        ContentBlock::WebSearchToolResult { content, .. } => rough(&content.to_string()),
    }
}

/// 一条消息的粗估。
pub(crate) fn message(message: &Message) -> u64 {
    match message {
        Message::User(user) => user.content.iter().map(block).sum(),
        Message::Assistant(assistant) => assistant.content.iter().map(block).sum(),
        // 内容块（`@` 提及的图片等）与消息里的块同样估算。
        Message::Attachment(attachment) => {
            rough(&attachment.text) + attachment.blocks().iter().map(block).sum::<u64>()
        }
        Message::System(_) => 0,
    }
}

pub(crate) fn messages<'a>(items: impl IntoIterator<Item = &'a Message>) -> u64 {
    items.into_iter().map(message).sum()
}

/// 一次响应的上下文大小（输入 + 缓存读写 + 输出）。
pub(crate) fn usage_total(usage: &Usage) -> u64 {
    usage.input_tokens
        + usage.cache_creation_input_tokens
        + usage.cache_read_input_tokens
        + usage.output_tokens
}

/// 最近一次带用量的响应 + 之后消息的粗估；遇到压缩边界时锚点是 0。
pub(crate) fn count(active: &[&Message]) -> u64 {
    for (index, item) in active.iter().enumerate().rev() {
        let anchor = match item {
            Message::System(SystemMessage::CompactBoundary(_)) => Some(0),
            Message::Assistant(assistant) => assistant.usage.as_ref().map(usage_total),
            _ => None,
        };
        if let Some(tokens) = anchor {
            return tokens + messages(active[index + 1..].iter().copied());
        }
    }
    messages(active.iter().copied())
}

/// `/context` 的消息测量（context-usage.md §4）：最近一条
/// 带用量、且输入 + 缓存读写大于 0 的响应的这三项之和，与它之后消息的估算。没有这样的响应时为 None。
pub fn measured(active: &[&Message]) -> Option<(u64, u64)> {
    active.iter().enumerate().rev().find_map(|(index, item)| {
        let Message::Assistant(assistant) = item else {
            return None;
        };
        let usage = assistant.usage.as_ref()?;
        let input =
            usage.input_tokens + usage.cache_creation_input_tokens + usage.cache_read_input_tokens;
        (input > 0).then(|| (input, messages(active[index + 1..].iter().copied())))
    })
}

#[cfg(test)]
#[path = "tokens_tests.rs"]
mod tests;
