//! 文字的通用规则（docs/specs/rust-cli/constants.md §2）：截断加省略号（超过上限时截到“上限 − 省略号长度”再加 `...`，
//! 结果不超过上限；以前 kernel-api 的会话标题、ext 的 skill 描述各写一份 N 与 N − 3），行 diff 的编辑距离上限（F122 收拢）。

use std::borrow::Cow;

use crate::js::string::{utf16_len, utf16_prefix};

/// 行 diff（Myers）的编辑距离上限：超过时不再求最小 diff（tools 的改动片段、session 回退计划的增删行数共用）。
pub const MAX_DIFF_EDITS: usize = 10_000;

/// 截断时加的省略号：三个 ASCII 点。
pub const ELLIPSIS: &str = "...";

/// 超过 `max` 个 UTF-16 码元时截到 `max − 3` 个再加 `...`；不超过时原样。截断点落在代理对中间时不拆字符。
pub fn truncate_utf16(text: &str, max: usize) -> Cow<'_, str> {
    if utf16_len(text) <= max {
        return Cow::Borrowed(text);
    }
    let head = utf16_prefix(text, max.saturating_sub(ELLIPSIS.len()));
    Cow::Owned(format!("{head}{ELLIPSIS}"))
}

/// 同 [`truncate_utf16`]，按字符（Unicode 标量）数。
pub fn truncate_chars(text: &str, max: usize) -> Cow<'_, str> {
    if text.chars().count() <= max {
        return Cow::Borrowed(text);
    }
    let head: String = text
        .chars()
        .take(max.saturating_sub(ELLIPSIS.len()))
        .collect();
    Cow::Owned(format!("{head}{ELLIPSIS}"))
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
