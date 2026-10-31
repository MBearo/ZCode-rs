//! 分组与切分（docs/specs/rust-cli/compact.md §3.1）。
//!
//! 按 API 轮分组、全部摘要的切分、摘要部分要有实质内容、PTL 后多保留几组、从头裁。

use rust_cli_kernel_api::message::{ContentBlock, Message, UserOrigin};

use crate::tokens;

/// 少于这么多组时没有可压缩的部分。
pub(crate) const MIN_GROUPS: usize = 2;
/// 没有初始差时先去掉这么多分之一的组（至少一组）。
const DEFAULT_DROP_SHARE: usize = 5;

/// 人类输入达到这个估算值时，没有 assistant 也算有实质内容。
const SUBSTANTIVE_HUMAN_TOKENS: u64 = 1_000;

/// 按 API 轮分组：每条 assistant 开一个新组；system 消息不发给模型，不参与分组。
pub(crate) fn group<'a>(messages: &[&'a Message]) -> Vec<Vec<&'a Message>> {
    let mut groups: Vec<Vec<&'a Message>> = Vec::new();
    let mut current: Vec<&'a Message> = Vec::new();
    for message in messages {
        if matches!(message, Message::System(_)) {
            continue;
        }
        if matches!(message, Message::Assistant(_)) && !current.is_empty() {
            groups.push(std::mem::take(&mut current));
        }
        current.push(message);
    }
    if !current.is_empty() {
        groups.push(current);
    }
    groups
}

pub(crate) fn flat<'a>(groups: &[Vec<&'a Message>]) -> Vec<&'a Message> {
    groups.iter().flatten().copied().collect()
}

pub(crate) fn has_assistant(messages: &[&Message]) -> bool {
    messages
        .iter()
        .any(|message| matches!(message, Message::Assistant(_)))
}

fn is_tool_result(message: &Message) -> bool {
    matches!(message, Message::User(user) if matches!(user.origin, UserOrigin::ToolResult { .. }))
}

/// 人类输入（user、非 meta、不是工具结果、不是摘要）。
pub(crate) fn is_human(message: &Message) -> bool {
    matches!(message, Message::User(user) if user.origin == UserOrigin::Human
        && !user.content.iter().any(|block| matches!(block, ContentBlock::ToolResult { .. })))
}

/// 摘要部分要有实质内容：有 assistant，或人类输入的估算 ≥ 1000。
pub(crate) fn substantive(messages: &[&Message]) -> bool {
    if has_assistant(messages) {
        return true;
    }
    let human: Vec<&Message> = messages.iter().copied().filter(|m| is_human(m)).collect();
    !human.is_empty() && tokens::messages(human) >= SUBSTANTIVE_HUMAN_TOKENS
}

/// 全部摘要的切分：最后一条 assistant / 工具结果之后的第一条人类输入及以后保留。
pub(crate) fn split_all<'a>(messages: &[&'a Message]) -> (Vec<&'a Message>, Vec<&'a Message>) {
    let last_reply = messages
        .iter()
        .rposition(|m| matches!(m, Message::Assistant(_)) || is_tool_result(m));
    let start = messages
        .iter()
        .enumerate()
        .rposition(|(index, m)| last_reply.is_none_or(|last| index > last) && is_human(m));
    match start {
        None => (messages.to_vec(), Vec::new()),
        Some(start) => (messages[..start].to_vec(), messages[start..].to_vec()),
    }
}

/// PTL 之后多保留几组：从摘要部分的末尾往前累计，到 token 差为止；几乎全要保留时取一半。
pub(crate) fn preserve_step(group_tokens: &[u64], summarize_end: usize, gap: Option<u64>) -> usize {
    let Some(gap) = gap else {
        return 1;
    };
    let mut sum = 0;
    let mut count = 0;
    for tokens in group_tokens[..summarize_end].iter().rev() {
        sum += tokens;
        count += 1;
        if sum >= gap {
            break;
        }
    }
    if count + 1 >= summarize_end {
        return (summarize_end / 2).max(1);
    }
    count
}

/// 从头裁掉若干组：按 token 差裁，不知道差多少时裁掉 20%；
/// 至少留一组。裁完以 assistant 开头时，调用方要补一条截断说明（返回值第二项为真）。
pub(crate) fn truncate_head<'a>(
    messages: &[&'a Message],
    gap: Option<u64>,
) -> Option<(Vec<&'a Message>, bool)> {
    let groups = group(messages);
    if groups.len() < MIN_GROUPS {
        return None;
    }
    let mut start = match gap {
        Some(gap) => {
            let mut removed = 0;
            let mut count = 0;
            for group in &groups {
                removed += tokens::messages(group.iter().copied());
                count += 1;
                if removed >= gap {
                    break;
                }
            }
            count
        }
        None => (groups.len() / DEFAULT_DROP_SHARE).max(1),
    };
    start = start.min(groups.len() - 1);
    if start < 1 {
        return None;
    }
    let retained = flat(&groups[start..]);
    let needs_marker = matches!(retained.first(), Some(Message::Assistant(_)));
    Some((retained, needs_marker))
}

#[cfg(test)]
#[path = "groups_tests.rs"]
mod tests;
