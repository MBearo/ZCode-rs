//! TUI 询问框“不再询问”一行的种子（可编辑的前缀，docs/specs/rust-cli/shell-permissions.md §4.10，工作线 F72）。
//!
//! 修复（D9）：以前这一行直接记住判决链的建议（取前两个词，`git -C /repo status` 记成 `git:*`）；现在按询问框
//! 自己的规则：复合命令用唯一的那条建议，否则简单前缀、第一个词或整条命令，再按规格细化。

use rust_cli_base::chars;
use rust_cli_kernel_api::permission::PermissionRule;

use crate::suggest::{first_word_prefix, simple_prefix, starts_with_wrapper};

/// 变体选择符（文字与 emoji 的呈现方式）：看不见，也不许进种子。
const PRESENTATION_SELECTORS: [char; 2] = ['\u{fe0e}', '\u{fe0f}'];

/// 种子里不能有的字符：换行、制表符、行与段分隔符、控制字符、双向控制符与零宽字符、变体选择符。
/// 修复（F122）：双向与零宽字符以前只拒绝 U+061C、U+202A–U+202E、U+2066–U+2069，零宽空格、LRM / RLM、
/// U+206A–U+206F、BOM 放过了；三处各写一份范围，现在用 base 的同一张表（constants.md §5.1.4）。
fn printable(seed: &str) -> bool {
    !seed.chars().any(|c| {
        chars::is_c0(c)
            || chars::is_del_or_c1(c)
            || chars::is_line_break_separator(c)
            || chars::is_bidi_or_zero_width(c)
            || PRESENTATION_SELECTORS.contains(&c)
    })
}

fn checked(seed: String) -> Option<String> {
    printable(&seed).then_some(seed)
}

/// Bash 询问框的种子；没有种子时询问框用固定文字的一行（多于一条建议）或不显示这一行（没有建议）。
pub fn rule_seed(command: &str, suggestions: &[PermissionRule]) -> Option<String> {
    let compound = crate::bash::subcommand_count(command) > 1;
    if compound {
        let rules: Vec<&str> = suggestions
            .iter()
            .filter(|rule| rule.tool == "Bash")
            .filter_map(|rule| rule.content.as_deref())
            .filter(|content| !content.is_empty())
            .collect();
        match rules.as_slice() {
            [only] => return checked((*only).to_owned()),
            [_, _, ..] => return None,
            [] => {}
        }
    }
    let seed = simple_prefix(command)
        .or_else(|| first_word_prefix(command))
        .map_or_else(|| command.to_owned(), |prefix| format!("{prefix} *"));
    if !compound
        && let Some(refined) = crate::suggest_spec::spec_prefix(command)
        && !starts_with_wrapper(&refined)
        && let Some(refined) = checked(format!("{refined} *"))
    {
        return Some(refined);
    }
    checked(seed)
}

/// 记住的规则显示时去掉末尾的 ` *` / `:*`（没有种子时的那一行）。
pub fn rule_label(content: &str) -> &str {
    content
        .strip_suffix(":*")
        .or_else(|| content.strip_suffix(" *"))
        .unwrap_or(content)
}

#[cfg(test)]
#[path = "suggest_seed_tests.rs"]
mod tests;
