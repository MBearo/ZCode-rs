//! 模型可见的 hook 文本（docs/specs/rust-cli/hooks.md §4.4）：attachment 渲染、Stop 反馈与
//! `<system-reminder` 的转义。

use rust_cli_kernel_api::clock::Ids;
use rust_cli_kernel_api::message::{
    AttachmentMessage, ContentBlock, Message, UserMessage, UserOrigin,
};
use rust_cli_prompts::reminder;
use serde_json::{Value, json};

use crate::result::Blocking;

/// PreToolUse 的 `continue: false`。
pub(crate) const STOPPED_BY_HOOK: &str = "Execution stopped by hook";
/// PostToolUse 的 `continue: false`（`ooe`）。
pub(crate) const STOPPED_BY_POST_TOOL_USE: &str = "Execution stopped by PostToolUse hook";
/// Stop 的 `continue: false`。
pub(crate) const STOP_PREVENTED: &str = "Stop hook prevented continuation";
/// UserPromptSubmit 的 `continue: false`。
pub(crate) const OPERATION_STOPPED: &str = "Operation stopped by hook";
/// PermissionRequest 拒绝时没有 message。
pub(crate) const PERMISSION_DENIED_BY_HOOK: &str = "Permission denied by hook";

/// 转义：`<system-reminder` 与 `</system-reminder` 前的 `<`、结尾单独的 `<` 转成 `&lt;`。
pub(crate) fn escape(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    for (index, ch) in text.char_indices() {
        if ch == '<' && opens_reminder(&lower[index + 1..]) {
            out.push_str("&lt;");
        } else {
            out.push(ch);
        }
    }
    // `<(?=\s*(?:\/\s*)?$)`：只替换第一个满足条件的位置（非全局正则）。
    if let Some(position) = trailing_lt(&out) {
        out.replace_range(position..=position, "&lt;");
    }
    out
}

/// `(?=\s*(?:\/\s*)?system-reminder\b)`。
fn opens_reminder(rest: &str) -> bool {
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('/').map_or(rest, str::trim_start);
    rest.strip_prefix("system-reminder").is_some_and(|after| {
        after
            .chars()
            .next()
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'))
    })
}

fn trailing_lt(text: &str) -> Option<usize> {
    text.char_indices()
        .filter(|(_, ch)| *ch == '<')
        .map(|(index, _)| index)
        .find(|index| {
            let rest = text[index + 1..].trim_start();
            let rest = rest.strip_prefix('/').map_or(rest, str::trim_start);
            rest.is_empty()
        })
}

fn attachment(ids: &dyn Ids, source: &str, text: &str, data: Value) -> Message {
    Message::Attachment(AttachmentMessage {
        id: ids.message(),
        source: source.to_owned(),
        text: reminder::wrap(&escape(text)),
        data,
    })
}

/// `hook_additional_context`；没有内容时不产生消息。
pub(crate) fn additional_context(
    ids: &dyn Ids,
    hook_name: &str,
    event: &str,
    contexts: &[String],
) -> Option<Message> {
    if contexts.is_empty() {
        return None;
    }
    let text = format!(
        "{hook_name} hook additional context: {}",
        contexts.join("\n")
    );
    Some(attachment(
        ids,
        "hook_additional_context",
        &text,
        json!({"hookName": hook_name, "hookEvent": event, "content": contexts}),
    ))
}

pub(crate) fn blocking_error(
    ids: &dyn Ids,
    hook_name: &str,
    event: &str,
    blocking: &Blocking,
) -> Message {
    let text = format!(
        "{hook_name} hook blocking error from command: \"{}\": {}",
        blocking.command, blocking.error
    );
    attachment(
        ids,
        "hook_blocking_error",
        &text,
        json!({
            "hookName": hook_name,
            "hookEvent": event,
            "blockingError": {"blockingError": blocking.error, "command": blocking.command},
        }),
    )
}

pub(crate) fn stopped_continuation(
    ids: &dyn Ids,
    hook_name: &str,
    event: &str,
    message: &str,
) -> Message {
    attachment(
        ids,
        "hook_stopped_continuation",
        &format!("{hook_name} hook stopped continuation: {message}"),
        json!({"hookName": hook_name, "hookEvent": event, "message": message}),
    )
}

/// `hook_success`：只在 SessionStart / UserPromptSubmit 且 content 非空时给模型看。
pub(crate) fn success(ids: &dyn Ids, hook_name: &str, event: &str, content: &str) -> Message {
    attachment(
        ids,
        "hook_success",
        &format!("{hook_name} hook success: {content}"),
        json!({"hookName": hook_name, "hookEvent": event, "content": content}),
    )
}

/// meta user 消息（`createUserMessage({isMeta: true})`），文本原样。
pub(crate) fn meta(ids: &dyn Ids, source: &str, text: String) -> Message {
    Message::User(UserMessage {
        id: ids.message(),
        content: vec![ContentBlock::Text { text }],
        origin: UserOrigin::Meta {
            source: source.to_owned(),
        },
    })
}

/// Stop 的阻塞反馈。
pub(crate) fn stop_feedback(ids: &dyn Ids, blocking: &Blocking) -> Message {
    meta(
        ids,
        "stop_hook_feedback",
        format!("Stop hook feedback:\n{}", escape(&blocking.error)),
    )
}

/// PreToolUse 阻塞的拒绝文本。
pub(crate) fn pre_tool_blocking(hook_name: &str, blocking: &Blocking) -> String {
    format!("{hook_name} hook error: {}", blocking.error)
}

/// PreToolUse 的 JSON 决定没有原因时的文本（`getRuleBehaviorDescription`）。
pub(crate) fn decision_default(hook_name: &str, verb: &str) -> String {
    format!("Hook {hook_name} {verb} this tool")
}

/// UserPromptSubmit 的阻塞说明。
pub(crate) fn prompt_blocked(blocking: &Blocking, prompt: &str, suppress: bool) -> String {
    let text = format!(
        "UserPromptSubmit operation blocked by hook:\n{}",
        blocking.error
    );
    if suppress {
        text
    } else {
        format!("{text}\n\nOriginal prompt: {prompt}")
    }
}

#[cfg(test)]
#[path = "texts_tests.rs"]
mod tests;
