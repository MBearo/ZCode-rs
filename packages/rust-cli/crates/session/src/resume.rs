//! 中断后续跑（docs/specs/rust-cli/session-lifecycle.md §6.3，工作线 F81 R3）。
//!
//! 冷加载的历史里，进程在工具执行中退出留下的 tool_use 没有结果（`open_operations` 里的操作结果未知，不重跑）：
//! - 末尾的（最后一条人类输入之后的回复里的）：留着，在历史最后为每个补一条 `is_error` 的中断结果；
//! - 更早的：只去掉这些 tool_use 块，同一回复的文字与 thinking 留着，回复空了才整条去掉。
//!
//! 补的结果只在内存里，由 actor 随第一批带消息的提交落库（同续写占位）。
//!
//! Node 写的未结调用不经过这里：存储冷读取时已在内存里给它们补好结果（只在内存、不写库，用户决定 2026-10-24，
//! session-lifecycle.md §6.3.2，工作线 F86），这里看到的是已有结果的调用，不补也不提交。
//!
//! 修 bug：以前去掉所有调用都没有结果的回复（文字与 thinking 一起丢），部分有结果的回复整条保留，请求里留着没有结果
//! 的 tool_use；也没有代码补缺的结果。

use std::collections::HashSet;

use rust_cli_kernel_api::ids::{MessageId, ToolUseId};
pub(crate) use rust_cli_kernel_api::message::INTERRUPTED_CALL;
use rust_cli_kernel_api::message::{
    ContentBlock, Message, ToolResultContent, UserMessage, UserOrigin,
};

fn calls(message: &Message) -> impl Iterator<Item = &ToolUseId> {
    let blocks: &[ContentBlock] = match message {
        Message::Assistant(assistant) => &assistant.content,
        _ => &[],
    };
    blocks.iter().filter_map(|block| match block {
        ContentBlock::ToolUse { id, .. } => Some(id),
        _ => None,
    })
}

fn has_result(user: &UserMessage) -> bool {
    user.content
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
}

/// 末尾的未结调用：从最后往前，越过 system、attachment 与带工具结果的 user；还没越过回复时也越过 meta 消息
/// （跟着工具调用的伴随消息，例如 skill 的正文）；遇到别的 user（人类输入）为止。越过的回复里没有结果的调用都算。
fn trailing(messages: &[Message], unresolved: &HashSet<&ToolUseId>) -> HashSet<ToolUseId> {
    let mut out = HashSet::new();
    let mut seen_reply = false;
    for message in messages.iter().rev() {
        match message {
            Message::System(_) | Message::Attachment(_) => {}
            Message::User(user) if has_result(user) => {}
            Message::User(user)
                if !seen_reply && matches!(user.origin, UserOrigin::Meta { .. }) => {}
            Message::User(_) => break,
            Message::Assistant(_) => {
                seen_reply = true;
                out.extend(calls(message).filter(|id| unresolved.contains(id)).cloned());
            }
        }
    }
    out
}

/// 处理没有结果的调用；返回新的历史与补出的结果消息的 ID（按历史里的顺序，都还没落库）。
pub(crate) fn settle(
    messages: Vec<Message>,
    mut id: impl FnMut() -> MessageId,
) -> (Vec<Message>, Vec<MessageId>) {
    let results: HashSet<&ToolUseId> = messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user) => Some(user.content.iter()),
            _ => None,
        })
        .flatten()
        .filter_map(|block| match block {
            ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id),
            _ => None,
        })
        .collect();
    let unresolved: HashSet<&ToolUseId> = messages
        .iter()
        .flat_map(calls)
        .filter(|id| !results.contains(id))
        .collect();
    if unresolved.is_empty() {
        return (messages, Vec::new());
    }
    let keep = trailing(&messages, &unresolved);
    let dropped: HashSet<ToolUseId> = unresolved
        .into_iter()
        .filter(|id| !keep.contains(*id))
        .cloned()
        .collect();
    // 补的结果按回复与块的顺序。
    let owed: Vec<(MessageId, ToolUseId)> = messages
        .iter()
        .flat_map(|message| calls(message).map(move |call| (message.id().clone(), call.clone())))
        .filter(|(_, call)| keep.contains(call))
        .collect();
    let mut out: Vec<Message> = messages
        .into_iter()
        .filter_map(|message| match message {
            Message::Assistant(mut assistant) => {
                let before = assistant.content.len();
                assistant.content.retain(|block| {
                    !matches!(block, ContentBlock::ToolUse { id, .. } if dropped.contains(id))
                });
                let emptied = assistant.content.is_empty() && before > 0;
                (!emptied).then_some(Message::Assistant(assistant))
            }
            other => Some(other),
        })
        .collect();
    let mut added = Vec::with_capacity(owed.len());
    for (assistant, call) in owed {
        let message = id();
        added.push(message.clone());
        out.push(Message::User(UserMessage {
            id: message,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: call,
                content: vec![ToolResultContent::Text {
                    text: INTERRUPTED_CALL.to_owned(),
                }],
                is_error: true,
            }],
            origin: UserOrigin::ToolResult { assistant },
        }));
    }
    (out, added)
}

#[cfg(test)]
#[path = "resume_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "resume_settle_tests.rs"]
mod settle_tests;
