//! 提醒并进工具结果（docs/specs/rust-cli/context.md §3 第 9 条）。
//!
//! 一段 user 角色的提醒（带 `<system-reminder>` 包装）要接到 user 消息末尾，而末尾是工具结果时，不另起
//! 文本块，而是把提醒并进这条工具结果。模型不支持对话中间的 system 消息时，每个工具轮的提醒都走这条路。
//! query 的 normalize 与 model 的 system 消息降级共用这里的规则。
//!
//! 缺陷修复（R4 F7）：原来两处都在工具结果后面另起一个文本块，缓存断点随之落在文本块上；正确的断点落在
//! 工具结果上。

// JS `String.prototype.trim`（比 Rust 的空白多 U+FEFF、少 U+0085）：唯一定义在 base（F58：以前这里写了一份
// `js_trim`，测试核对两种写法逐个 char 相同）。
use rust_cli_base::js::string::trim as js_trim;

use crate::ids::ToolUseId;
use crate::message::{ContentBlock, ToolResultContent};
use crate::model::ApiContent;

/// 留在结果之外的署名提醒（两种开头）。
const ATTRIBUTION: [&str; 2] = [
    "<system-reminder>\nAttribution for git commits and pull requests you create from here on",
    "<system-reminder>\nFrom here on, do not add attribution lines to git commit messages or pull request descriptions",
];

/// 这条工具结果在请求里是不是块数组（不是字符串）。形状取决于工具：Agent、SendMessage 与 MCP 工具的
/// 正常结果是块数组，出错的结果与其余工具是字符串。内核不保存这个形式
/// （tools-files.md D6），按工具名推。
pub fn array_result(tool_name: &str, is_error: bool) -> bool {
    !is_error
        && (tool_name == "Agent" || tool_name == "SendMessage" || tool_name.starts_with("mcp__"))
}

/// 把 `reminder` 并进 `content` 末尾的工具结果；并进了返回 true，调用方不再另起文本块。
/// `array_form(结果的 tool_use_id, is_error)`：这条结果在请求里是不是块数组（见 [`array_result`]）。
///
/// - 末尾不是工具结果、结果里有 `tool_reference`（ToolSearch）、提醒是署名提醒：不并，返回 false；
/// - 字符串形式（内核里是空或单个文本块）：两段各去首尾空白，非空的用空行连接；
/// - 块数组：最后一个文本块补一个换行，提醒原样成为下一个块。
///
/// 结果上已有的缓存断点不动；断点由调用方在整条请求上统一放。
pub fn fold_into_tool_result(
    content: &mut [ApiContent],
    reminder: &str,
    array_form: impl Fn(&ToolUseId, bool) -> bool,
) -> bool {
    if ATTRIBUTION.iter().any(|head| reminder.starts_with(head)) {
        return false;
    }
    let Some(ContentBlock::ToolResult {
        tool_use_id,
        content: result,
        is_error,
    }) = content.last_mut().map(|item| &mut item.block)
    else {
        return false;
    };
    if result
        .iter()
        .any(|item| matches!(item, ToolResultContent::ToolReference { .. }))
    {
        return false;
    }
    let single_text = matches!(result.as_slice(), [] | [ToolResultContent::Text { .. }]);
    if single_text && !array_form(tool_use_id, *is_error) {
        let previous = match result.first() {
            Some(ToolResultContent::Text { text }) => js_trim(text),
            _ => "",
        };
        let joined = [previous, js_trim(reminder)]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        *result = vec![ToolResultContent::Text { text: joined }];
    } else {
        if let Some(ToolResultContent::Text { text }) = result.last_mut() {
            text.push('\n');
        }
        result.push(ToolResultContent::Text {
            text: reminder.to_owned(),
        });
    }
    true
}

#[cfg(test)]
#[path = "reminder_fold_tests.rs"]
mod tests;
