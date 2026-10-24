//! 按这一步的输入算的提醒（docs/specs/rust-cli/context.md §4.4）：`ultrathink_effort` 与 `skill_mention`。
//!
//! “这一步的输入”只在主线程、最后一条非 attachment 消息是这次输入时有：人类输入（不是命令标签、不是 bash
//! 模式），或用户调用 skill 展开的正文。工具轮之后、通知与定时任务开的回合都没有。

use rust_cli_kernel_api::message::{ContentBlock, Message, UserMessage, UserOrigin};
use rust_cli_kernel_api::{user_command, user_input};
use rust_cli_prompts::texts::{self, render};
use serde_json::{Value, json};

pub(crate) const ULTRATHINK: &str = "ultrathink_effort";
pub(crate) const SKILL_MENTION: &str = "skill_mention";

/// skill 正文的 meta 来源。
const SKILL_SOURCE: &str = "skill";
const KEYWORD: &[u8] = b"ultrathink";

/// 这一步的输入。
pub(crate) enum TurnInput {
    /// 人类键入的普通输入（文字块以换行连接）。
    Typed(String),
    /// 用户调用 skill 展开的正文（文字块以空格连接，context.md §4.4）。
    SkillBody(String),
}

impl TurnInput {
    fn text(&self) -> &str {
        match self {
            Self::Typed(text) | Self::SkillBody(text) => text,
        }
    }
}

fn texts_of(user: &UserMessage, separator: &str) -> String {
    user.content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(separator)
}

/// 命令标签或 bash 模式的人类消息（不是普通输入）。
fn is_command(user: &UserMessage) -> bool {
    user.content.iter().any(|block| {
        matches!(block, ContentBlock::Text { text }
            if user_command::display(text).is_some()
                || user_input::display(text).is_some()
                || user_input::output_parts(text).is_some())
    })
}

/// 这一步的输入；不是输入那一步时为 None。
pub(crate) fn turn_input(history: &[Message]) -> Option<TurnInput> {
    let mut messages = history
        .iter()
        .rev()
        .filter(|message| !matches!(message, Message::Attachment(_)));
    let Message::User(last) = messages.next()? else {
        return None;
    };
    match &last.origin {
        UserOrigin::Human if !is_command(last) => Some(TurnInput::Typed(texts_of(last, "\n"))),
        UserOrigin::Meta { source } if source == SKILL_SOURCE => {
            // 用户调用的 skill：前一条是命令标签；模型经 Skill 工具调用的（前一条是工具结果）不算。
            let Some(Message::User(command)) = messages.next() else {
                return None;
            };
            (command.origin == UserOrigin::Human && is_command(command))
                .then(|| TurnInput::SkillBody(texts_of(last, " ")))
        }
        _ => None,
    }
}

/// JS `\w`（不带 `u` 标志只认 ASCII）。
fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// `/\bultrathink\b/i`：前后是 ASCII 单词边界，ASCII 不分大小写（不带 `u` 标志的 `i` 不把非 ASCII 折成 ASCII）。
pub(crate) fn has_ultrathink(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes
        .windows(KEYWORD.len())
        .enumerate()
        .any(|(at, window)| {
            window.eq_ignore_ascii_case(KEYWORD)
                && (at == 0 || !is_word(bytes[at - 1]))
                && bytes
                    .get(at + KEYWORD.len())
                    .is_none_or(|next| !is_word(*next))
        })
}

/// `ultrathink_effort`：输入里有这个词时的提醒。
pub(crate) fn ultrathink(input: &TurnInput) -> Option<(String, Value)> {
    has_ultrathink(input.text()).then(|| (texts::ultrathink_effort().to_owned(), Value::Null))
}

/// 人类键入的输入里要找 skill 提及的文字（用户调用的 skill 不找）。
pub(crate) fn typed(input: &TurnInput) -> Option<&str> {
    match input {
        TurnInput::Typed(text) => Some(text),
        TurnInput::SkillBody(_) => None,
    }
}

/// `skill_mention`：一个 skill 一条。
pub(crate) fn skill_mention(name: &str) -> (String, Value) {
    (
        render(texts::skill_mention(), &[("name", name)]),
        json!({ "skillName": name }),
    )
}

#[cfg(test)]
#[path = "input_reminders_tests.rs"]
mod tests;
