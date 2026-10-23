//! 活动历史：组装模型上下文时用的那一段消息（docs/specs/rust-cli/compact.md §5）。
//!
//! 历史只追加，压缩前的消息不删除。活动历史只有这一个定义，与 Node `compactActiveSessionMessages`
//! （node-store 冷读取）相同：
//!
//! 1. 从最后一个压缩边界开始，含边界本身；
//! 2. 边界带保留段时，在边界之前找 `head..=tail`，去掉 system 消息，assistant 的用量清零
//!    （Node `invalidateRuntimeTokenUsage` 相同），插在 `anchor`（摘要）之后；
//! 3. 冷读取的结果已是活动形状（保留段在边界之后），再套一次不变。

use std::borrow::Cow;

use crate::message::{CompactBoundary, Message, SystemMessage};

/// 最后一个压缩边界的位置与内容。
pub fn last_boundary(history: &[Message]) -> Option<(usize, &CompactBoundary)> {
    history
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, message)| match message {
            Message::System(SystemMessage::CompactBoundary(boundary)) => Some((index, boundary)),
            _ => None,
        })
}

/// 保留段里的消息：assistant 的用量属于被替换的前缀，清零后不再作为 token 估算的锚点。
fn preserved_copy(message: &Message) -> Cow<'_, Message> {
    match message {
        Message::Assistant(assistant) if assistant.usage.is_some() => {
            let mut copy = assistant.clone();
            copy.usage = None;
            Cow::Owned(Message::Assistant(copy))
        }
        other => Cow::Borrowed(other),
    }
}

/// 活动历史。没有边界时就是全部历史。
pub fn active(history: &[Message]) -> Vec<Cow<'_, Message>> {
    let Some((start, boundary)) = last_boundary(history) else {
        return history.iter().map(Cow::Borrowed).collect();
    };
    let mut out: Vec<Cow<'_, Message>> = history[start..].iter().map(Cow::Borrowed).collect();
    let Some(segment) = &boundary.preserved else {
        return out;
    };
    let before = &history[..start];
    let find = |id: &crate::ids::MessageId| before.iter().position(|m| m.id() == id);
    let (Some(head), Some(tail)) = (find(&segment.head), find(&segment.tail)) else {
        // 冷读取：保留段已经接在摘要之后。
        return out;
    };
    if tail < head {
        return out;
    }
    let preserved: Vec<Cow<'_, Message>> = before[head..=tail]
        .iter()
        .filter(|message| !matches!(message, Message::System(_)))
        .map(preserved_copy)
        .collect();
    let insert = out
        .iter()
        .position(|message| message.id() == &segment.anchor)
        .map_or(1, |index| index + 1)
        .min(out.len());
    out.splice(insert..insert, preserved);
    out
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
