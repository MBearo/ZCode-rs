//! 中断后续跑（docs/specs/rust-cli/session-lifecycle.md §6.3）。
//!
//! 冷加载的历史里，进程在工具执行中退出留下的 tool_use 没有结果（`open_operations` 里的操作结果
//! 未知，不重跑）。去掉所有 tool_use 都没有结果的 assistant；
//! 部分有结果的保留，缺的结果由主循环 normalize 补。只改内存，库里保持原样。

use std::collections::HashSet;

use rust_cli_kernel_api::message::{ContentBlock, Message};

pub(crate) fn filter_unresolved(messages: Vec<Message>) -> Vec<Message> {
    let mut uses = HashSet::new();
    let mut results = HashSet::new();
    for message in &messages {
        let blocks = match message {
            Message::User(user) => &user.content,
            Message::Assistant(assistant) => &assistant.content,
            _ => continue,
        };
        for block in blocks {
            match block {
                ContentBlock::ToolUse { id, .. } => {
                    uses.insert(id.clone());
                }
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    results.insert(tool_use_id.clone());
                }
                _ => {}
            }
        }
    }
    if uses.iter().all(|id| results.contains(id)) {
        return messages;
    }
    messages
        .into_iter()
        .filter(|message| {
            let Message::Assistant(assistant) = message else {
                return true;
            };
            let mut calls = assistant.content.iter().filter_map(|block| match block {
                ContentBlock::ToolUse { id, .. } => Some(id),
                _ => None,
            });
            let mut any = false;
            let unresolved = calls.all(|id| {
                any = true;
                !results.contains(id)
            });
            !(any && unresolved)
        })
        .collect()
}

#[cfg(test)]
#[path = "resume_tests.rs"]
mod tests;
