//! TUI 询问框“不再询问”一行的种子（可编辑的前缀，docs/specs/rust-cli/shell-permissions.md §4.10，工作线 F72）。
//!
//! 修复（D9）：以前这一行直接记住判决链的建议（取前两个词，`git -C /repo status` 记成 `git:*`）；现在按询问框
//! 自己的规则：复合命令用唯一的那条建议，否则简单前缀、第一个词或整条命令，再按规格细化。

use rust_cli_kernel_api::permission::PermissionRule;

use crate::suggest::{first_word_prefix, simple_prefix, starts_with_wrapper};

/// 种子里不能有的字符：换行、制表符、行与段分隔符、控制字符、双向控制符与变体选择符。
fn printable(seed: &str) -> bool {
    !seed.chars().any(|c| {
        let code = u32::from(c);
        code < 0x20
            || (0x7f..=0x9f).contains(&code)
            || matches!(code, 0x2028 | 0x2029 | 0x061c | 0xfe0e | 0xfe0f)
            || (0x202a..=0x202e).contains(&code)
            || (0x2066..=0x2069).contains(&code)
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
