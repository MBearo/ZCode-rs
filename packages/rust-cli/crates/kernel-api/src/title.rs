//! 会话标题的公共规则（docs/specs/rust-cli/titles.md §4）。node-store 写 `first_input` 标题、会话 actor
//! 更新内存里的标题都用这里的函数（唯一定义）。

use rust_cli_base::js::string::collapse_spaces;
use rust_cli_base::text;

/// 没有可用标题时的缺省（Node `titleFromInput` 的空输入）：首条输入为空、fork 的原会话还没有标题时都用它（F55：
/// 以前这里、session 与 node-store 的 fork 各写一份）。
pub const UNTITLED: &str = "Untitled session";
/// 临时标题最多这么多个 UTF-16 码元（超过时截断加 `...`，titles.md §4）。
const MAX_TITLE_UNITS: usize = 60;

/// Node `titleFromInput`：首条输入的临时标题（`first_input`）。`trim` 后把 `\s+` 压成一个空格；为空时
/// `Untitled session`；超过 60 个 UTF-16 码元时截到 57 个再加 `...`。
///
/// 已知差异：截断点落在代理对中间时，Node 留下孤立的代理项，这里不拆字符（少半个字符）。
pub fn from_input(input: &str) -> String {
    // 压空白只定义在 base（F58：以前这里、session 与 base 的 `sanitize_message` 各写一份）。
    let compact = collapse_spaces(input);
    if compact.is_empty() {
        return UNTITLED.into();
    }
    // 截断加省略号只定义在 base 的 `text`。
    text::truncate_utf16(&compact, MAX_TITLE_UNITS).into_owned()
}

#[cfg(test)]
#[path = "title_tests.rs"]
mod tests;
