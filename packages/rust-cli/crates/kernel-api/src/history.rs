//! 活动历史：组装模型上下文时用的那一段消息（docs/specs/rust-cli/compact.md §5）。
//!
//! 历史只追加，压缩前的消息不删除。活动历史只有这一个定义，与 Node `compactActiveSessionMessages`
//! （node-store 冷读取）相同：
//!
//! 1. 从最后一个压缩边界开始，含边界本身；
//! 2. 边界带保留段时，在边界之前找 `head..=tail`，去掉 system 消息，assistant 的用量清零
//!    （Node `invalidateRuntimeTokenUsage` 相同），插在 `anchor`（摘要）之后；
//! 3. 冷读取的结果已是活动形状（保留段在边界之后），再套一次不变。
//!
//! 另有“历史里成功的 Read / Write / Edit 碰过哪些文件”（[`touched_files`]，工作线 F132 的 T9）：嵌套的规则文件
//! （context.md §11.6）与条件 skill（ext.md §2.6）都从历史推导，以前各写一份。

use std::borrow::Cow;
use std::collections::HashSet;

use serde_json::Value;

use crate::message::{CompactBoundary, ContentBlock, Message, SystemMessage};

/// 成功的调用算“碰过这个文件”的工具（Read、Write、Edit；NotebookEdit 的路径在 `notebook_path`，不算）。
pub const FILE_TOOLS: [&str; 3] = [
    crate::tool_names::READ,
    crate::tool_names::WRITE,
    crate::tool_names::EDIT,
];

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

/// 一个 tool_use 碰的文件：工具是 [`FILE_TOOLS`] 之一、`file_path` 非空时给出原始路径（不解析、不规范化）。
pub fn touched_file<'a>(name: &str, input: &'a Value) -> Option<&'a str> {
    if !FILE_TOOLS.contains(&name) {
        return None;
    }
    input
        .get("file_path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
}

/// `history` 里成功的 Read / Write / Edit 调用碰过的文件：结果也在 `history` 里、不是错误。原始路径，按调用顺序，
/// 不去重。
pub fn touched_files(history: &[Message]) -> Vec<&str> {
    let mut answered = HashSet::new();
    let mut failed = HashSet::new();
    for message in history {
        let Message::User(user) = message else {
            continue;
        };
        for block in &user.content {
            if let ContentBlock::ToolResult {
                tool_use_id,
                is_error,
                ..
            } = block
            {
                answered.insert(tool_use_id);
                if *is_error {
                    failed.insert(tool_use_id);
                }
            }
        }
    }
    let mut paths = Vec::new();
    for message in history {
        let Message::Assistant(assistant) = message else {
            continue;
        };
        for block in &assistant.content {
            let ContentBlock::ToolUse { id, name, input } = block else {
                continue;
            };
            if let Some(path) = touched_file(name, input)
                && answered.contains(id)
                && !failed.contains(id)
            {
                paths.push(path);
            }
        }
    }
    paths
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "history_touched_tests.rs"]
mod touched_tests;
