//! token 估算：消息的粗估，与带用量的响应加之后消息的粗估。算法只有一份，在 kernel-api（`compaction::message_tokens`、
//! `context_tokens`）：目录模型请求的输出预算（docs/specs/rust-cli/query.md §1.11）用同一个函数，修复线 F108 从这里
//! 搬过去。

use rust_cli_kernel_api::compaction::message_tokens;
use rust_cli_kernel_api::message::Message;

/// 图片与文档的估算值。
pub use rust_cli_kernel_api::compaction::IMAGE_TOKENS;

/// 字符数 / 4，四舍五入；字符数按 UTF-16 码元（JS 的 `length`）。唯一定义在 kernel-api（压缩后带回的 skill 也用它）。
pub(crate) fn rough(text: &str) -> u64 {
    rust_cli_kernel_api::compaction::rough_tokens(text)
}

pub(crate) fn messages<'a>(items: impl IntoIterator<Item = &'a Message>) -> u64 {
    items.into_iter().map(message_tokens).sum()
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
