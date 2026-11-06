//! `/context` 类别的明细（docs/specs/rust-cli/context-usage.md §3.1）：消息按内容块分类，类别的数按估算的比例用最大
//! 余数法分给各项，每个列表的和等于它的类别。只用本地估算，不发请求。

use std::collections::HashMap;

use rust_cli_kernel_api::context_usage::{MessageUsage, ToolResultUsage};
use rust_cli_kernel_api::hooks::queued_command;
use rust_cli_kernel_api::message::{AttachmentMessage, ContentBlock, Message, UserOrigin};
use serde_json::Value;

use super::counts;

/// 找不到调用的结果的工具名。
const UNKNOWN_TOOL: &str = "Unknown";

/// 消息的种类，按明细里的顺序。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Yours,
    Replies,
    Thinking,
    Calls,
    Results,
    Attachments,
    Summary,
}

const KINDS: [Kind; 7] = [
    Kind::Yours,
    Kind::Replies,
    Kind::Thinking,
    Kind::Calls,
    Kind::Results,
    Kind::Attachments,
    Kind::Summary,
];

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Yours => "Your messages",
            Self::Replies => "Replies",
            Self::Thinking => "Thinking",
            Self::Calls => "Tool calls",
            Self::Results => "Tool results",
            Self::Attachments => "Attachments",
            Self::Summary => "Summary",
        }
    }
}

/// 一个内容块：种类、结果所属的工具（只有 `Tool results`）与估算（权重）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Piece {
    pub(crate) kind: Kind,
    pub(crate) tool: Option<String>,
    pub(crate) weight: u64,
}

/// 最大余数法：`total` 按 `weights` 的比例分开，和等于 `total`。每项先得整数部分，剩下的按余数从大到小每项加 1，
/// 余数相同时按原来的顺序。权重全是 0 时各项按 1。
pub(crate) fn apportion(total: u64, weights: &[u64]) -> Vec<u64> {
    if weights.is_empty() {
        return Vec::new();
    }
    let mut weights: Vec<u128> = weights.iter().copied().map(u128::from).collect();
    if weights.iter().all(|weight| *weight == 0) {
        weights.fill(1);
    }
    let sum: u128 = weights.iter().sum();
    let whole = u128::from(total);
    let mut shares: Vec<u64> = weights
        .iter()
        .map(|weight| u64::try_from(whole * weight / sum).unwrap_or(u64::MAX))
        .collect();
    let mut order: Vec<usize> = (0..weights.len()).collect();
    // 稳定排序：余数相同时保持原来的顺序。
    order.sort_by_key(|index| std::cmp::Reverse(whole * weights[*index] % sum));
    let left = total - shares.iter().sum::<u64>();
    for index in order
        .into_iter()
        .take(usize::try_from(left).unwrap_or(usize::MAX))
    {
        shares[index] += 1;
    }
    shares
}

/// 人类插话（`queued_command`、`commandMode: prompt`）：是人打的字，算 `Your messages`。
fn is_human_prompt(attachment: &AttachmentMessage) -> bool {
    attachment.source == queued_command::SOURCE
        && attachment
            .data
            .get(queued_command::MODE_KEY)
            .and_then(Value::as_str)
            == Some(queued_command::PROMPT_MODE)
}

fn assistant_kind(block: &ContentBlock) -> Kind {
    match block {
        ContentBlock::Text { .. } | ContentBlock::Image { .. } => Kind::Replies,
        ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. } => Kind::Thinking,
        ContentBlock::ToolUse { .. } | ContentBlock::ServerToolUse { .. } => Kind::Calls,
        ContentBlock::ToolResult { .. } | ContentBlock::WebSearchToolResult { .. } => Kind::Results,
    }
}

/// 活动历史的内容块按种类分开（§3.1 的表）。`messages` 是估算消息的同一组会话消息（去掉了不发给模型的 attachment）；
/// 按规范化之前的消息分类，因为规范化会合并 user 消息、把提醒并进工具结果，合并之后分不出来源。
pub(crate) fn pieces(messages: &[&Message], per: u64) -> Vec<Piece> {
    let mut names: HashMap<&str, &str> = HashMap::new();
    for message in messages {
        if let Message::Assistant(assistant) = message {
            for block in &assistant.content {
                match block {
                    ContentBlock::ToolUse { id, name, .. } => {
                        names.insert(id.as_str(), name);
                    }
                    ContentBlock::ServerToolUse { id, name, .. } => {
                        names.insert(id, name);
                    }
                    _ => {}
                }
            }
        }
    }
    let piece = |block: &ContentBlock, kind: Kind| {
        let called = match block {
            ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
            ContentBlock::WebSearchToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
            _ => None,
        };
        Piece {
            kind: if called.is_some() {
                Kind::Results
            } else {
                kind
            },
            tool: called.map(|id| names.get(id).copied().unwrap_or(UNKNOWN_TOOL).to_owned()),
            weight: counts::block(block, per),
        }
    };
    let mut out = Vec::new();
    for message in messages {
        match message {
            Message::User(user) => {
                let kind = match user.origin {
                    UserOrigin::Human => Kind::Yours,
                    UserOrigin::ToolResult { .. } | UserOrigin::Meta { .. } => Kind::Attachments,
                    UserOrigin::CompactSummary => Kind::Summary,
                };
                out.extend(user.content.iter().map(|block| piece(block, kind)));
            }
            Message::Assistant(assistant) => out.extend(
                assistant
                    .content
                    .iter()
                    .map(|block| piece(block, assistant_kind(block))),
            ),
            // 文字为空的 attachment 只供显示，不发给模型（与规范化相同）。
            Message::Attachment(attachment) if attachment.text.is_empty() => {}
            Message::Attachment(attachment) => {
                let kind = if is_human_prompt(attachment) {
                    Kind::Yours
                } else {
                    Kind::Attachments
                };
                out.push(Piece {
                    kind,
                    tool: None,
                    weight: counts::rough(&attachment.text, per),
                });
                out.extend(attachment.blocks().iter().map(|block| piece(block, kind)));
            }
            Message::System(_) => {}
        }
    }
    out
}

/// 消息的种类与每个工具的结果：`total` 是 `Messages` 类别（可能是锚点测得的数），按种类的权重分开；
/// `Tool results` 那一份再按工具的权重分开。0 的项不列。
pub(crate) fn messages(total: u64, pieces: &[Piece]) -> (Vec<MessageUsage>, Vec<ToolResultUsage>) {
    let kinds: Vec<(Kind, u64)> = KINDS
        .iter()
        .filter(|kind| pieces.iter().any(|piece| piece.kind == **kind))
        .map(|kind| {
            let weight = pieces
                .iter()
                .filter(|piece| piece.kind == *kind)
                .map(|piece| piece.weight)
                .sum();
            (*kind, weight)
        })
        .collect();
    let weights: Vec<u64> = kinds.iter().map(|(_, weight)| *weight).collect();
    let shares = apportion(total, &weights);
    let results = kinds
        .iter()
        .zip(&shares)
        .find(|((kind, _), _)| *kind == Kind::Results)
        .map_or(0, |(_, share)| *share);
    let messages = kinds
        .iter()
        .zip(shares)
        .filter(|(_, tokens)| *tokens > 0)
        .map(|((kind, _), tokens)| MessageUsage {
            kind: kind.name().to_owned(),
            tokens,
        })
        .collect();
    // 每个工具：（名字，个数，权重），按第一次出现的顺序。
    let mut tools: Vec<(&str, u64, u64)> = Vec::new();
    for piece in pieces {
        let Some(tool) = &piece.tool else { continue };
        match tools.iter_mut().find(|(name, _, _)| *name == tool) {
            Some(found) => {
                found.1 += 1;
                found.2 += piece.weight;
            }
            None => tools.push((tool, 1, piece.weight)),
        }
    }
    let weights: Vec<u64> = tools.iter().map(|(_, _, weight)| *weight).collect();
    let tool_results = tools
        .iter()
        .zip(apportion(results, &weights))
        .filter(|(_, tokens)| *tokens > 0)
        .map(|((name, calls, _), tokens)| ToolResultUsage {
            tool: (*name).to_owned(),
            calls: *calls,
            tokens,
        })
        .collect();
    (messages, tool_results)
}

#[cfg(test)]
#[path = "detail_tests.rs"]
mod tests;
