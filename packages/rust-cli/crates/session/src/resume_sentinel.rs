//! 续写时补 `No response requested.`（docs/specs/rust-cli/session-lifecycle.md §6.3.1）。
//!
//! 冷加载的历史以一条没有回答的 user 消息结束（被中断、进程中途退出、工具结果之后还没有回复）时，在它后面补一条
//! assistant，之后的新输入不会和旧输入并成一条。界面不显示。随第一批带消息的提交落库（`Actor::commit`），之后每次
//! 冷加载都在原处；只打开不续写时不写。会话里切过去时（`/resume`、`/fork`）界面发 `Command::StoreResumeSentinel`，
//! 马上单独写一批（`Actor::store_resume_sentinel`，工作线 F79）。

use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, NO_RESPONSE_REQUESTED, SYNTHETIC_MODEL, StopReason,
    UserMessage, UserOrigin,
};
use rust_cli_kernel_api::user_input::is_interrupt;

/// 本地命令记录里的一行属于哪一段：命令本身、输出、caveat。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalPart {
    Record,
    Output,
    Caveat,
}

fn last_text(user: &UserMessage) -> Option<&str> {
    user.content.iter().rev().find_map(|block| match block {
        ContentBlock::Text { text } => Some(text.as_str()),
        _ => None,
    })
}

fn is_meta(user: &UserMessage) -> bool {
    matches!(user.origin, UserOrigin::Meta { .. })
}

/// 按最后一个文字块的开头归类；不是本地命令的行为 None。caveat 只有 meta 的才算。
fn local_part(message: &Message) -> Option<LocalPart> {
    let Message::User(user) = message else {
        return None;
    };
    let text = last_text(user)?;
    let part = [
        ("<command-name>", LocalPart::Record),
        ("<local-command-stdout>", LocalPart::Output),
        ("<local-command-stderr>", LocalPart::Output),
        ("<local-command-caveat>", LocalPart::Caveat),
    ]
    .into_iter()
    .find_map(|(prefix, part)| text.starts_with(prefix).then_some(part))?;
    (part != LocalPart::Caveat || is_meta(user)).then_some(part)
}

/// `index` 是本地命令的一段记录：往前走过输出行、到一条命令行为止，遇到 meta 的 caveat，或走过的行都是 meta。
fn local_command_chain(messages: &[Message], index: usize) -> bool {
    if local_part(&messages[index]).is_none() {
        return false;
    }
    let (mut seen_record, mut all_meta) = (false, true);
    for message in messages[..=index].iter().rev() {
        if matches!(message, Message::System(_) | Message::Attachment(_)) {
            continue;
        }
        let part = local_part(message);
        if part == Some(LocalPart::Caveat) {
            return true;
        }
        if part.is_none() || seen_record {
            break;
        }
        seen_record = part == Some(LocalPart::Record);
        all_meta &= matches!(message, Message::User(user) if is_meta(user));
    }
    all_meta
}

/// 以 attachment 结尾时：往前越过 attachment，落在一条还在等回复的 user 消息上（输入、工具结果）才算没有回答。
fn unanswered_before_attachments(messages: &[Message], index: usize) -> bool {
    for at in (0..=index).rev() {
        match &messages[at] {
            Message::System(_) | Message::Attachment(_) => {}
            Message::Assistant(_) => return false,
            Message::User(user) => {
                return !(user.origin == UserOrigin::CompactSummary
                    || is_interrupt(user)
                    || local_command_chain(messages, at));
            }
        }
    }
    true
}

/// 补的 assistant。
pub(crate) fn sentinel(id: MessageId) -> Message {
    Message::Assistant(AssistantMessage {
        id,
        content: vec![ContentBlock::Text {
            text: NO_RESPONSE_REQUESTED.to_owned(),
        }],
        model: SYNTHETIC_MODEL.to_owned(),
        stop_reason: Some(StopReason::StopSequence),
        usage: None,
    })
}

/// 要补时在最后一条非 system 消息之后补上（其后的 system 消息留在它后面），返回补的那条的 ID（还没落库）。
/// `last_turn_failed`：当前轮以错误结束（API 错误不落库，结束时的错误就是历史的最后一条），不补。
pub(crate) fn insert(
    messages: &mut Vec<Message>,
    last_turn_failed: bool,
    id: impl FnOnce() -> MessageId,
) -> Option<MessageId> {
    if last_turn_failed {
        return None;
    }
    let last = messages
        .iter()
        .rposition(|message| !matches!(message, Message::System(_)))?;
    let unanswered = match &messages[last] {
        Message::User(_) => !local_command_chain(messages, last),
        Message::Attachment(_) => unanswered_before_attachments(messages, last),
        Message::Assistant(_) | Message::System(_) => false,
    };
    if !unanswered {
        return None;
    }
    let id = id();
    messages.insert(last + 1, sentinel(id.clone()));
    Some(id)
}

#[cfg(test)]
#[path = "resume_sentinel_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "resume_persist_tests.rs"]
mod persist_tests;
