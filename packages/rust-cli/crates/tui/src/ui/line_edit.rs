//! 单行输入的编辑（架构整理 U12，用户决定 Q8，docs/specs/rust-cli/tui-input.md §5）：只在末尾增删（与原来一样），
//! backspace 删掉最后一个字素，粘贴时换行压成空格。
//!
//! 缺陷修复：以前 15 个单行输入各写 `String::pop`，组合字符（`e` 加组合的锐音符）与带肤色的表情只删掉一半；列表的过滤、
//! 详细视图与 ← 项目页的搜索、输入历史的搜索、`/permissions` 的过滤不收粘贴（丢掉，或进了被盖住的输入框）。
use unicode_segmentation::UnicodeSegmentation;

/// 删掉最后一个字素；空的时候返回 false。
pub fn backspace(text: &mut String) -> bool {
    match text.grapheme_indices(true).next_back() {
        Some((at, _)) => {
            text.truncate(at);
            true
        }
        None => false,
    }
}

/// 在末尾粘贴：换行（`\r\n`、`\n`、`\r`）压成一个空格。
pub fn paste(text: &mut String, pasted: &str) {
    text.push_str(&pasted.replace("\r\n", " ").replace(['\n', '\r'], " "));
}

#[cfg(test)]
#[path = "line_edit_tests.rs"]
mod tests;
