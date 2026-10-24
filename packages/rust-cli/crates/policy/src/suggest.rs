//! “同类命令不再询问”的建议规则：一条子命令的建议、简单前缀、第一个词与包装表
//! （docs/specs/rust-cli/shell-permissions.md §4.10，工作线 F72）。
//!
//! 修复（D9）：以前取前两个词、第二个词不像子命令时退到第一个词，`git -C /repo status` 建议成过宽的
//! `git:*`；现在第二个词不像子命令就给精确规则，前缀规则写成 `<前缀> *`。

use rust_cli_base::js::string::is_space;
use rust_cli_kernel_api::permission::PermissionRule;

use crate::safe_env;

const TOOL: &str = "Bash";

/// 后面接任意命令的包装与 shell：按前缀放行它们等于放行任意命令，不给前缀。
pub(crate) const WRAPPERS: [&str; 37] = [
    "sh",
    "bash",
    "zsh",
    "fish",
    "csh",
    "tcsh",
    "ksh",
    "dash",
    "cmd",
    "powershell",
    "pwsh",
    "env",
    "xargs",
    "command",
    "builtin",
    "noglob",
    "nice",
    "stdbuf",
    "nohup",
    "timeout",
    "time",
    "watch",
    "ionice",
    "chrt",
    "setsid",
    "taskset",
    "strace",
    "ltrace",
    "script",
    "flock",
    "unshare",
    "nsenter",
    "sudo",
    "doas",
    "pkexec",
    "su",
    "runuser",
];

/// 按 JS 的 `\s+` 拆词（`trim().split(/\s+/).filter(Boolean)`）。
pub(crate) fn words(text: &str) -> Vec<&str> {
    text.split(is_space)
        .filter(|word| !word.is_empty())
        .collect()
}

/// `^[A-Za-z_]\w*=`：命令前的变量赋值。
pub(crate) fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `^[a-z][a-z0-9]*(-[a-z0-9]+)*$`：像子命令的词。
pub(crate) fn is_subcommand_word(word: &str) -> bool {
    let mut parts = word.split('-');
    let Some(head) = parts.next() else {
        return false;
    };
    let mut head_chars = head.chars();
    head_chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && head_chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && parts.all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

/// 命令名按 `/` 之后的部分查包装表。
pub(crate) fn is_wrapper(name: &str) -> bool {
    let base = name.rsplit('/').next().unwrap_or(name);
    WRAPPERS.contains(&base)
}

/// 跳过开头的安全环境变量赋值；遇到表外的变量名时没有结果。
fn skip_safe_assignments<'a>(words: &'a [&'a str]) -> Option<&'a [&'a str]> {
    let mut start = 0;
    while let Some(word) = words.get(start).filter(|word| is_assignment(word)) {
        let name = word.split('=').next().unwrap_or_default();
        if !safe_env::is_safe(name) {
            return None;
        }
        start += 1;
    }
    Some(&words[start..])
}

/// 简单前缀：命令名加一个像子命令的词。
pub(crate) fn simple_prefix(command: &str) -> Option<String> {
    let all = words(command);
    let rest = skip_safe_assignments(&all)?;
    if rest.len() < 2 || is_wrapper(rest[0]) || !is_subcommand_word(rest[1]) {
        return None;
    }
    Some(rest[..2].join(" "))
}

/// 第一个词（只给询问框的种子用）：像子命令的词、不是包装。
pub(crate) fn first_word_prefix(command: &str) -> Option<String> {
    let all = words(command);
    let first = *skip_safe_assignments(&all)?.first()?;
    (is_subcommand_word(first) && !WRAPPERS.contains(&first)).then(|| first.to_owned())
}

/// `<<` 之前的部分的前缀（heredoc 与 here-string）。
fn heredoc_prefix(command: &str) -> Option<String> {
    let at = command.find("<<").filter(|at| *at > 0)?;
    let before = command[..at].trim_matches(is_space);
    if before.is_empty() {
        return None;
    }
    if let Some(prefix) = simple_prefix(before) {
        return Some(prefix);
    }
    let all = words(before);
    let rest = skip_safe_assignments(&all)?;
    let first = rest.first()?;
    if is_wrapper(first) {
        return None;
    }
    Some(rest[..rest.len().min(2)].join(" "))
}

fn prefix_rule(prefix: &str) -> PermissionRule {
    PermissionRule {
        tool: TOOL.to_owned(),
        content: Some(format!("{prefix} *")),
    }
}

/// 一条子命令的建议规则：heredoc 之前的前缀、多行时的第一行、简单前缀，否则精确规则。
pub fn suggestion(command: &str) -> PermissionRule {
    if let Some(prefix) = heredoc_prefix(command) {
        return prefix_rule(&prefix);
    }
    if command.contains('\n') {
        let first = command
            .split('\n')
            .next()
            .unwrap_or_default()
            .trim_matches(is_space);
        if !first.is_empty() {
            return prefix_rule(first);
        }
    }
    if let Some(prefix) = simple_prefix(command) {
        return prefix_rule(&prefix);
    }
    PermissionRule {
        tool: TOOL.to_owned(),
        content: Some(command.to_owned()),
    }
}

/// 去掉开头的变量赋值之后，第一个词是不是包装（询问框的细化结果不用包装开头的）。
pub(crate) fn starts_with_wrapper(prefix: &str) -> bool {
    words(prefix)
        .into_iter()
        .find(|word| !is_assignment(word))
        .is_some_and(is_wrapper)
}

#[cfg(test)]
#[path = "suggest_tests.rs"]
mod tests;
