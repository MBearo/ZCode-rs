//! micro 压缩：清理旧的工具结果（docs/specs/rust-cli/compact.md §9）。
//!
//! 保留最近的工具结果，清理更早的。
//! 只在服务端 `context_hint` 被拒时调用它，`context_hint` 默认关闭，所以默认配置下不发生；这里只
//! 提供算法，不接线。

use std::collections::HashSet;

use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::message::{ContentBlock, Message, ToolResultContent};

use crate::tokens;

/// 清理后的文本。
pub const CLEARED: &str = "[Old tool result content cleared]";
/// 已落盘的结果。
const PERSISTED_PREFIX: &str = "<persisted-output>";
/// 省下的估算不足这么多时不做。
pub const MIN_SAVINGS: u64 = 20_000;
/// 保留最近几个调用的结果（`context_hint` 路径）。
pub const KEEP_RECENT: usize = 5;
/// 可清理的工具。
const COMPACTABLE: [&str; 8] = [
    "Read",
    "Bash",
    "Grep",
    "Glob",
    "WebSearch",
    "WebFetch",
    "Edit",
    "Write",
];

/// 要清理的调用与省下的估算 token。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub cleared: Vec<ToolUseId>,
    pub tokens_saved: u64,
}

fn already_cleared(content: &[ToolResultContent]) -> bool {
    matches!(content, [ToolResultContent::Text { text }]
        if text == CLEARED || text.starts_with(PERSISTED_PREFIX))
}

pub fn plan(messages: &[&Message], keep_recent: usize) -> Plan {
    let calls: Vec<&ToolUseId> = messages
        .iter()
        .filter_map(|m| match m {
            Message::Assistant(assistant) => Some(assistant.content.iter()),
            _ => None,
        })
        .flatten()
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, name, .. } if COMPACTABLE.contains(&name.as_str()) => {
                Some(id)
            }
            _ => None,
        })
        .collect();
    let keep = keep_recent.max(1);
    let clear: HashSet<&ToolUseId> = calls[..calls.len().saturating_sub(keep)]
        .iter()
        .copied()
        .collect();
    let mut out = Plan::default();
    for message in messages {
        let Message::User(user) = message else {
            continue;
        };
        for block in &user.content {
            if let ContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } = block
                && clear.contains(tool_use_id)
                && !already_cleared(content)
            {
                out.tokens_saved += content
                    .iter()
                    .map(|item| match item {
                        ToolResultContent::Text { text } => tokens::rough(text),
                        ToolResultContent::Image { .. } | ToolResultContent::Document { .. } => {
                            tokens::IMAGE_TOKENS
                        }
                        ToolResultContent::ToolReference { tool_name } => tokens::rough(tool_name),
                    })
                    .sum::<u64>();
                out.cleared.push(tool_use_id.clone());
            }
        }
    }
    out
}

/// 按计划清理；省下的不足 [`MIN_SAVINGS`] 时返回 None（不改消息）。
pub fn apply(messages: &[Message], keep_recent: usize) -> Option<(Vec<Message>, Plan)> {
    let refs: Vec<&Message> = messages.iter().collect();
    let plan = plan(&refs, keep_recent);
    if plan.tokens_saved < MIN_SAVINGS {
        return None;
    }
    let cleared: HashSet<&ToolUseId> = plan.cleared.iter().collect();
    let out = messages
        .iter()
        .map(|message| match message {
            Message::User(user) => {
                let mut copy = user.clone();
                for block in &mut copy.content {
                    if let ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } = block
                        && cleared.contains(tool_use_id)
                    {
                        *content = vec![ToolResultContent::Text {
                            text: CLEARED.to_owned(),
                        }];
                    }
                }
                Message::User(copy)
            }
            other => other.clone(),
        })
        .collect();
    Some((out, plan))
}

#[cfg(test)]
#[path = "micro_tests.rs"]
mod tests;
