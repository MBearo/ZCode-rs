//! `/context` 各部分的数（docs/specs/rust-cli/context-usage.md §3）：只用本地估算，不发模型请求。
//!
//! 修改（工作线 F93，用户决定 2026-10-24）：以前经模型端口的 count_tokens 计数，失败或不支持时才本地估算，成功的
//! 结果按请求内容缓存；现在一律本地估算（计数失败时的同一套，D13），计数、缓存与只为计数服务的会话字段一起删掉。

use rust_cli_base::tokens;
use rust_cli_kernel_api::message::ContentBlock;
use rust_cli_kernel_api::model::{ApiContent, ApiMessage, ApiRole, ToolDefinition};

/// 每次计数附带的工具开销：只有工具的那几项减去它才是工具本身。
pub(crate) const TOOL_OVERHEAD: u64 = 500;

/// 一次 `/context` 的估算器：每 token 字符数按模型（`rust_cli_prompts::models::chars_per_token`）。
pub(crate) struct Counter {
    pub(crate) per_token: u64,
}

/// 一段文字作为一条 user 消息。
pub(crate) fn user_text(text: &str) -> Vec<ApiMessage> {
    vec![ApiMessage {
        role: ApiRole::User,
        content: vec![ApiContent {
            block: ContentBlock::Text {
                text: text.to_owned(),
            },
            cache_control: None,
        }],
        output_config: None,
    }]
}

/// JS `Math.round(text.length / per)`，长度按 UTF-16（估算只定义在 base 的 `tokens`）。
pub(crate) fn rough(text: &str, per: u64) -> u64 {
    tokens::estimate(text.encode_utf16().count() as u64, per)
}

/// 本地估算（context-usage.md §3）：消息按内容块（字符数 / 每 token 字符数，图片 2000，其他块按 JSON 长度），有工具时加
/// JSON 长度 / 2 与开销。没有消息时消息部分是 0（计数请求的占位 `foo` 不算）。每 token 字符数按模型（修 bug（F10，
/// context-usage.md D5）：以前一律按 4）。
fn estimate(messages: &[ApiMessage], tools: &[ToolDefinition], per: u64) -> u64 {
    let mut total: u64 = messages
        .iter()
        .flat_map(|message| &message.content)
        .map(|content| match &content.block {
            ContentBlock::Text { text } => rough(text, per),
            // 每张图的估计与压缩共用（2000，F55）。
            ContentBlock::Image { .. } => rust_cli_compact::IMAGE_TOKENS,
            other => rough(&serde_json::to_string(other).unwrap_or_default(), per),
        })
        .sum();
    if !tools.is_empty() {
        let json = serde_json::to_string(tools).unwrap_or_default();
        total += rough(&json, tokens::JSON_CHARS_PER_TOKEN) + TOOL_OVERHEAD;
    }
    total
}

impl Counter {
    /// 一组消息与工具。
    pub(crate) fn count(&self, messages: &[ApiMessage], tools: &[ToolDefinition]) -> u64 {
        estimate(messages, tools, self.per_token)
    }

    /// 一段文字（system 段落、规则文件、agent、MCP instructions）。
    pub(crate) fn text(&self, text: &str) -> u64 {
        self.count(&user_text(text), &[])
    }

    /// 一组工具（没有消息）。
    pub(crate) fn tools(&self, tools: &[ToolDefinition]) -> u64 {
        self.count(&[], tools)
    }
}

#[cfg(test)]
#[path = "counts_tests.rs"]
mod tests;
