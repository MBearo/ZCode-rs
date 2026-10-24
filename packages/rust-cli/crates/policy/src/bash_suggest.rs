//! 整条 Bash 命令的建议规则：哪些命令不给建议、子命令的文本、按管道分段的汇总
//! （docs/specs/rust-cli/shell-permissions.md §4.10，工作线 F72）。

use std::collections::HashSet;

use rust_cli_kernel_api::permission::PermissionRule;

use crate::check::{AskKind, Check};
use crate::parse::{Analysis, Operator};
use crate::suggest::suggestion;

/// 汇总时每段最多几条。
const MAX_PER_SEGMENT: usize = 5;

/// 第一个 heredoc 的定界词带不带引号（`<<'EOF'`、`<<\EOF` 带）；没有 heredoc 时为 None。here-string `<<<` 不算。
fn heredoc_quoted(text: &str) -> Option<bool> {
    let bytes = text.as_bytes();
    let at = (0..bytes.len().saturating_sub(1)).find(|&index| {
        bytes[index] == b'<'
            && bytes[index + 1] == b'<'
            && bytes.get(index + 2) != Some(&b'<')
            && (index == 0 || bytes[index - 1] != b'<')
    })?;
    let rest = text[at + 2..].trim_start_matches('-');
    let word = rest
        .trim_start_matches([' ', '\t'])
        .split([' ', '\t', '\n', ';', '&', '|'])
        .next()
        .unwrap_or_default();
    Some(word.contains(['\'', '"', '\\']))
}

/// 单引号以外的参数展开或命令替换（`$((…))` 算术展开与 `\$` 不算）。
fn has_expansion(text: &str) -> bool {
    let mut chars = text.chars().peekable();
    let (mut single, mut double) = (false, false);
    while let Some(c) = chars.next() {
        if single {
            single = c != '\'';
            continue;
        }
        match c {
            '\\' => {
                chars.next();
            }
            '\'' if !double => single = true,
            '"' => double = !double,
            '`' => return true,
            '$' => match chars.peek().copied() {
                Some('(') => {
                    chars.next();
                    if chars.peek() != Some(&'(') {
                        return true;
                    }
                }
                Some(next)
                    if next == '{'
                        || next == '_'
                        || next.is_ascii_alphanumeric()
                        || "@*#?$!-".contains(next) =>
                {
                    return true;
                }
                _ => {}
            },
            _ => {}
        }
    }
    false
}

/// 不给建议的命令：解析在后台 `&` 以外的复合结构处停下、有不带引号的 heredoc、有参数展开或命令替换。
pub(crate) fn withheld(command: &str, analysis: &Analysis) -> bool {
    if analysis
        .unsupported_node_types
        .iter()
        .any(|kind| kind != "background")
    {
        return true;
    }
    let texts: Vec<&str> = if analysis.commands.is_empty() {
        vec![command]
    } else {
        analysis
            .commands
            .iter()
            .map(|invocation| invocation.command_text.as_str())
            .collect()
    };
    texts
        .iter()
        .any(|text| heredoc_quoted(text) == Some(false) || has_expansion(text))
}

/// 建议用的子命令文本：续行连同两边的空格换成一个空格，去掉带引号的 heredoc 一段。
pub(crate) fn subcommand_text(text: &str) -> String {
    let mut joined = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("\\\n") {
        joined.push_str(rest[..at].trim_end_matches([' ', '\t']));
        joined.push(' ');
        rest = rest[at + 2..].trim_start_matches([' ', '\t']);
    }
    joined.push_str(rest);
    if heredoc_quoted(&joined) == Some(true)
        && let Some(at) = joined.find("<<")
    {
        let after = joined[at + 2..].trim_start_matches('-');
        let after = after.trim_start_matches([' ', '\t']);
        let end = after.find([' ', '\t', '\n']).unwrap_or(after.len());
        joined = format!("{} {}", joined[..at].trim_end(), after[end..].trim_start());
    }
    joined.trim().to_owned()
}

/// 一条子命令在汇总里的建议：“需要批准”给它的建议，路径等询问没有自己的建议时也给，ask 规则不给。
fn of_subcommand(text: &str, check: &Check) -> Vec<PermissionRule> {
    match check {
        Check::Allow | Check::Deny { .. } => Vec::new(),
        Check::Ask {
            kind: AskKind::Rule,
            suggestions,
            ..
        } => suggestions.clone(),
        Check::Ask { suggestions, .. } | Check::Passthrough { suggestions, .. }
            if !suggestions.is_empty() =>
        {
            suggestions.clone()
        }
        Check::Ask { .. } | Check::Passthrough { .. } => vec![suggestion(&subcommand_text(text))],
    }
}

/// 汇总：按管道分段，一段只有一条子命令时用它自己的建议，多条时去重、最多 5 条；各段接起来。
pub(crate) fn combine(parts: &[(String, Option<Operator>, Check)]) -> Vec<PermissionRule> {
    let mut segments: Vec<Vec<&(String, Option<Operator>, Check)>> = Vec::new();
    for part in parts {
        let starts = matches!(part.1, Some(Operator::Pipe | Operator::PipeAll));
        match segments.last_mut() {
            Some(segment) if !starts => segment.push(part),
            _ => segments.push(vec![part]),
        }
    }
    let mut all = Vec::new();
    for segment in segments {
        if let [(_, _, check)] = segment.as_slice() {
            all.extend(match check {
                Check::Ask { suggestions, .. } | Check::Passthrough { suggestions, .. } => {
                    suggestions.clone()
                }
                Check::Allow | Check::Deny { .. } => Vec::new(),
            });
            continue;
        }
        let mut seen = HashSet::new();
        let combined = segment
            .iter()
            .flat_map(|(text, _, check)| of_subcommand(text, check))
            .filter(|rule| seen.insert((rule.tool.clone(), rule.content.clone())))
            .take(MAX_PER_SEGMENT);
        all.extend(combined);
    }
    all
}

#[cfg(test)]
#[path = "bash_suggest_tests.rs"]
mod tests;
