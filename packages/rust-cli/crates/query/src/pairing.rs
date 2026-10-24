//! 请求前的配对兜底（docs/specs/rust-cli/query.md §1.8，context.md §3 第 12 条）：normalize 合并之后，补缺的工具
//! 结果，去掉孤立与重复的结果和重复的 tool_use。正常的历史不会触发它（执行器给每个调用一个结果，续写时补中断
//! 结果）；它挡住的是存储里留下的不配对（例如以前的版本续写过被杀掉的会话），这种请求会被 API 拒绝。

use std::collections::HashSet;

use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::message::{ContentBlock, ToolResultContent};
use rust_cli_kernel_api::model::{ApiContent, ApiMessage, ApiRole};

use crate::texts;

fn plain(block: ContentBlock) -> ApiContent {
    ApiContent {
        block,
        cache_control: None,
    }
}

fn text(value: &str) -> ApiContent {
    plain(ContentBlock::Text {
        text: value.to_owned(),
    })
}

fn user(content: Vec<ApiContent>) -> ApiMessage {
    ApiMessage {
        role: ApiRole::User,
        content,
        output_config: None,
    }
}

fn is_result(item: &ApiContent) -> bool {
    matches!(item.block, ContentBlock::ToolResult { .. })
}

fn is_thinking(item: Option<&ApiContent>) -> bool {
    matches!(
        item.map(|item| &item.block),
        Some(ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. })
    )
}

/// 缺结果的调用补的错误结果。
fn missing(id: &ToolUseId) -> ApiContent {
    plain(ContentBlock::ToolResult {
        tool_use_id: id.clone(),
        content: vec![ToolResultContent::Text {
            text: texts::MISSING_TOOL_RESULT.to_owned(),
        }],
        is_error: true,
    })
}

/// assistant 的块：去掉已出现过的 tool_use、没有结果引用的服务端调用与放错地方的 tool_result；去掉的块前后都是
/// thinking 时换成说明文字，一个块都不剩时放一个说明块。返回新的块与这条回复里接受的调用。
fn repair_assistant(
    content: Vec<ApiContent>,
    seen: &mut HashSet<ToolUseId>,
) -> (Vec<ApiContent>, Vec<ToolUseId>) {
    let referenced: HashSet<&str> = content
        .iter()
        .filter_map(|item| match &item.block {
            ContentBlock::WebSearchToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
            _ => None,
        })
        .collect();
    let mut accepted = Vec::new();
    let mut kept = Vec::with_capacity(content.len());
    for (index, item) in content.iter().enumerate() {
        let remove = match &item.block {
            ContentBlock::ToolUse { id, .. } => {
                let repeated = !seen.insert(id.clone());
                if !repeated {
                    accepted.push(id.clone());
                }
                repeated
            }
            ContentBlock::ServerToolUse { id, .. } => !referenced.contains(id.as_str()),
            ContentBlock::ToolResult { .. } => true,
            _ => false,
        };
        if !remove {
            kept.push(item.clone());
        } else if index > 0
            && is_thinking(content.get(index - 1))
            && is_thinking(content.get(index + 1))
        {
            kept.push(text(texts::TOOL_USE_REMOVED));
        }
    }
    if kept.is_empty() {
        kept.push(text(texts::TOOL_USE_INTERRUPTED));
    }
    (kept, accepted)
}

/// 紧跟回复的 user 消息：缺的结果补在最前，不属于这条回复的结果与重复的结果去掉。
fn repair_results(next: ApiMessage, accepted: &[ToolUseId]) -> ApiMessage {
    let expected: HashSet<&ToolUseId> = accepted.iter().collect();
    let observed: HashSet<&ToolUseId> = next
        .content
        .iter()
        .filter_map(|item| match &item.block {
            ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id),
            _ => None,
        })
        .collect();
    let mut content: Vec<ApiContent> = accepted
        .iter()
        .filter(|id| !observed.contains(id))
        .map(missing)
        .collect();
    let mut retained: HashSet<ToolUseId> = HashSet::new();
    content.extend(
        next.content
            .iter()
            .filter(|item| match &item.block {
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    expected.contains(tool_use_id) && retained.insert(tool_use_id.clone())
                }
                _ => true,
            })
            .cloned(),
    );
    if content.is_empty() {
        content.push(text(texts::NO_CONTENT));
    }
    ApiMessage { content, ..next }
}

/// 这条 user 消息的结果与回复配对：没有缺的、多的或重复的。
fn paired(next: &ApiMessage, accepted: &[ToolUseId]) -> bool {
    let mut observed = Vec::new();
    for item in &next.content {
        if let ContentBlock::ToolResult { tool_use_id, .. } = &item.block {
            if observed.contains(&tool_use_id) {
                return false;
            }
            observed.push(tool_use_id);
        }
    }
    observed.len() == accepted.len() && accepted.iter().all(|id| observed.contains(&id))
}

pub(crate) fn ensure_pairing(messages: Vec<ApiMessage>) -> Vec<ApiMessage> {
    let mut out: Vec<ApiMessage> = Vec::with_capacity(messages.len());
    let mut seen: HashSet<ToolUseId> = HashSet::new();
    let mut rest = messages.into_iter().peekable();
    while let Some(mut message) = rest.next() {
        if message.role != ApiRole::Assistant {
            let after_reply = out
                .last()
                .is_some_and(|last| last.role == ApiRole::Assistant);
            if message.role == ApiRole::User
                && !after_reply
                && message.content.iter().any(is_result)
            {
                // 不跟在回复后面的结果去掉；没有别的内容时，第一条换成说明文字，其余整条去掉。
                let kept: Vec<ApiContent> = message
                    .content
                    .iter()
                    .filter(|item| !is_result(item))
                    .cloned()
                    .collect();
                if !kept.is_empty() {
                    out.push(ApiMessage {
                        content: kept,
                        ..message
                    });
                } else if out.is_empty() {
                    out.push(ApiMessage {
                        content: vec![text(texts::ORPHANED_TOOL_RESULT)],
                        ..message
                    });
                }
                continue;
            }
            out.push(message);
            continue;
        }
        let (content, accepted) = repair_assistant(std::mem::take(&mut message.content), &mut seen);
        out.push(ApiMessage { content, ..message });
        match rest.peek() {
            Some(next) if next.role == ApiRole::User => {
                if !paired(next, &accepted)
                    && let Some(next) = rest.next()
                {
                    out.push(repair_results(next, &accepted));
                }
            }
            _ if !accepted.is_empty() => out.push(user(accepted.iter().map(missing).collect())),
            _ => {}
        }
    }
    out
}
