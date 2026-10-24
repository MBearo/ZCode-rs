//! 静默回合提醒（`silent_turn_reminder`）：回合中途连续几轮都没有让用户看到东西时，提醒模型
//! 说明进展。按模型开启（模型卡片 `silent_turn_reminder`）。

use rust_cli_kernel_api::message::{AssistantMessage, ContentBlock, Message, UserOrigin};
use rust_cli_prompts::texts;
use serde_json::Value;

/// 来源（attachment type）。
pub(crate) const SOURCE: &str = "silent_turn_reminder";
/// 自上次提醒以来没有可见输出的轮数达到这个值才提醒（缺省值）。
const TURNS: usize = 5;
/// 一段静默里最多提醒几次。
const MAX_REMINDERS: usize = 3;
/// 让用户看到内容的工具（本工程目前有其中的 AskUserQuestion 与 ExitPlanMode）。
const VISIBLE_TOOLS: [&str; 2] = ["AskUserQuestion", "ExitPlanMode"];
/// 不算可见文本的占位（kernel-api 的定义）。
const NO_CONTENT: &str = rust_cli_kernel_api::message::NO_CONTENT;

fn visible(assistant: &AssistantMessage) -> bool {
    assistant.content.iter().any(|block| match block {
        ContentBlock::Text { text } => {
            let text = text.trim();
            !text.is_empty() && text != NO_CONTENT
        }
        ContentBlock::ToolUse { name, .. } => VISIBLE_TOOLS.contains(&name.as_str()),
        _ => false,
    })
}

/// （自上次提醒以来的静默轮数，这段静默里的提醒次数）：从后往前数，遇到可见输出或人类输入就停。
fn stretch(history: &[Message]) -> (usize, usize) {
    let (mut turns, mut reminders) = (0, 0);
    for message in history.iter().rev() {
        match message {
            Message::Assistant(assistant) => {
                if visible(assistant) {
                    break;
                }
                if reminders == 0 {
                    turns += 1;
                }
            }
            Message::Attachment(attachment) if attachment.source == SOURCE => reminders += 1,
            Message::User(user) if user.origin == UserOrigin::Human => break,
            _ => {}
        }
    }
    (turns, reminders)
}

/// 这一步要不要提醒：只在回合中途（最后一条非 attachment 消息是工具结果）判断。
pub(crate) fn announce(history: &[Message], enabled: bool) -> Option<(String, Value)> {
    let after_tools = matches!(
        history
            .iter()
            .rev()
            .find(|message| !matches!(message, Message::Attachment(_))),
        Some(Message::User(user)) if matches!(user.origin, UserOrigin::ToolResult { .. })
    );
    if !enabled || !after_tools {
        return None;
    }
    let (turns, reminders) = stretch(history);
    (turns >= TURNS && reminders < MAX_REMINDERS)
        .then(|| (texts::silent_turn().to_owned(), Value::Null))
}

#[cfg(test)]
#[path = "silent_tests.rs"]
mod tests;
