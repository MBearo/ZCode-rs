//! Bash 规则的匹配（shell-permissions.md §4.2、§4.5.6）。
//!
//! content 的三种形状与比较（前缀、通配、并空白）是 kernel-api `permission::syntax` 的纯字符串运算，与 hooks 的 `if`
//! 条件共用（F125）。这里定判决链拿哪些候选去比：命令原文、去掉重定向的命令，以及去掉安全的前缀赋值与包装命令之后的
//! 命令（[`crate::shell::strip`]）；deny 与 ask 规则另外去掉全部前缀赋值与 argv 层的包装命令（[`crate::shell::argv`]）。
//! 前缀与通配规则不匹配复合命令（复合命令按子命令分别匹配）。

use std::collections::HashSet;

use rust_cli_kernel_api::permission::RuleBehavior;
use rust_cli_kernel_api::permission::syntax::{
    ShellRule, ends_with_wildcard, prefix_matches, squeeze, wildcard_matches,
};

use crate::shell::strip::{strip_all_assignments, strip_safe_wrappers};

/// 规则与命令怎样比较。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 只比完整命令。
    Exact,
    /// 前缀与通配（单个子命令）。
    Prefix,
}

/// 候选的来源：一条命令的原文与解析结果（§4.5.6 的表）。
#[derive(Debug, Clone, Copy, Default)]
pub struct Subject<'a> {
    /// 原文。
    pub text: &'a str,
    /// 去掉重定向的原文；None 时同原文。
    pub plain: Option<&'a str>,
    /// argv（解析结果；没有时 None）。
    pub argv: Option<&'a [String]>,
    /// 有解析树而且有前缀赋值（argv 层去包装之后命令名没变也算候选）。
    pub assigned: bool,
}

impl<'a> Subject<'a> {
    #[cfg(test)]
    pub fn text(text: &'a str) -> Self {
        Self {
            text,
            ..Self::default()
        }
    }
}

/// 不动点里最多的候选（异常输入的上限；正常命令几轮就停）。
const MAX_CANDIDATES: usize = 64;

/// 规则比较的候选（§4.5.6）。`behavior` 是 allow 时只去安全的前缀；deny 与 ask 去掉全部前缀赋值与 argv 层的包装命令。
///
/// 修复（工作线 F85）：以前 deny 规则也只去安全名单里的前缀赋值与五个包装命令，`X=1 rm -f x`、`env rm`、`sudo rm`、
/// `\rm` 都比不上 `Bash(rm:*)`，guarded 与 yolo 下直接执行。
pub fn candidates(subject: Subject<'_>, mode: Mode, behavior: RuleBehavior) -> Vec<String> {
    let text = subject.text.trim().to_owned();
    let plain = subject
        .plain
        .map_or_else(|| text.clone(), |plain| plain.trim().to_owned());
    let bases = match mode {
        Mode::Exact => vec![text, plain],
        Mode::Prefix => vec![plain],
    };
    let mut found: Vec<String> = Vec::new();
    for base in bases {
        let stripped = strip_safe_wrappers(&base);
        let changed = stripped != base;
        found.push(base);
        if changed {
            found.push(stripped);
        }
    }
    if behavior == RuleBehavior::Allow {
        return found;
    }
    if let Some(argv) = subject.argv {
        let stripped = crate::shell::argv::strip_wrappers(argv);
        if let Some(first) = stripped.first()
            && (argv.first() != Some(first) || subject.assigned)
        {
            found.push(stripped.join(" "));
        }
    }
    let mut seen: HashSet<String> = found.iter().cloned().collect();
    let mut start = 0;
    while start < found.len() && found.len() < MAX_CANDIDATES {
        let end = found.len();
        for index in start..end {
            for next in [
                strip_all_assignments(&found[index]),
                strip_safe_wrappers(&found[index]),
            ] {
                if seen.insert(next.clone()) {
                    found.push(next);
                }
            }
        }
        start = end;
    }
    found
}

/// 一条 content 是否匹配候选之一（§4.5.6 的比较）。
pub fn matches(content: &str, candidates: &[String], mode: Mode, behavior: RuleBehavior) -> bool {
    let rule = ShellRule::parse(content);
    candidates.iter().any(|command| match (rule, mode) {
        (ShellRule::Exact(exact), _) => exact == command,
        (ShellRule::Prefix(prefix), Mode::Exact) => squeeze(prefix) == squeeze(command),
        (ShellRule::Wildcard(_), Mode::Exact) => false,
        (ShellRule::Prefix(prefix), Mode::Prefix) => prefix_matches(prefix, command),
        (ShellRule::Wildcard(pattern), Mode::Prefix) => {
            wildcard_matches(pattern, command)
                || ((behavior != RuleBehavior::Allow || ends_with_wildcard(pattern))
                    && wildcard_matches(&format!("xargs {pattern}"), command))
        }
    })
}

#[cfg(test)]
#[path = "shell_rules_tests.rs"]
mod tests;
