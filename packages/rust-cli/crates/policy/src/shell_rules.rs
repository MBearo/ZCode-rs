//! Bash 规则的匹配（shell-permissions.md §4.2、§4.5.6）。
//!
//! content 有三种：`git status:*` 是前缀（前缀后必须是结尾或空格），含未转义 `*` 的是通配，其余
//! 精确相等。规则与一组候选比较：命令原文、去掉重定向的命令，以及去掉安全的前缀赋值与包装命令之后的命令
//! （[`crate::shell_strip`]）；deny 与 ask 规则另外去掉全部前缀赋值与 argv 层的包装命令（[`crate::shell_argv`]）。
//! 前缀与通配规则不匹配复合命令（复合命令按子命令分别匹配）。

use std::collections::HashSet;

use regex::Regex;
use rust_cli_kernel_api::permission::RuleBehavior;

use crate::shell_strip::strip_all_assignments;
pub use crate::shell_strip::strip_safe_wrappers;

/// 一条 Bash 规则的 content。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellRule<'a> {
    Exact(&'a str),
    Prefix(&'a str),
    Wildcard(&'a str),
}

/// `*` 前面的反斜杠是偶数个（没有转义）。
fn unescaped_star(text: &str, index: usize) -> bool {
    text[..index]
        .bytes()
        .rev()
        .take_while(|byte| *byte == b'\\')
        .count()
        % 2
        == 0
}

fn has_wildcard(pattern: &str) -> bool {
    if pattern.ends_with(":*") {
        return false;
    }
    pattern
        .char_indices()
        .any(|(index, c)| c == '*' && unescaped_star(pattern, index))
}

/// 去掉末尾空白后以未转义的 `*` 结尾（allow 的通配规则这时也认 `xargs`）。
fn ends_with_wildcard(pattern: &str) -> bool {
    let pattern = pattern.trim_end();
    pattern.ends_with('*') && unescaped_star(pattern, pattern.len() - 1)
}

/// 解析一条 Bash 规则。
pub fn parse(content: &str) -> ShellRule<'_> {
    if let Some(prefix) = content
        .strip_suffix(":*")
        .filter(|prefix| !prefix.is_empty())
    {
        return ShellRule::Prefix(prefix);
    }
    if has_wildcard(content) {
        ShellRule::Wildcard(content)
    } else {
        ShellRule::Exact(content)
    }
}

/// 连续的空格、制表符并成一个空格。
fn squeeze(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank = false;
    for c in text.chars() {
        if matches!(c, ' ' | '\t') {
            if !blank {
                out.push(' ');
            }
            blank = true;
        } else {
            out.push(c);
            blank = false;
        }
    }
    out
}

/// `/**/` 里 `/` 之后的一段（可以连着几段）。
const GLOBSTAR_SEGMENT: &str = "**/";
/// 唯一的 `*` 在结尾 ` *` 时翻译出的正则尾巴（这时它可以整个省略）。
const TRAILING_ANY: &str = " .*";

/// 通配匹配：`*` 匹配任意文本，`\*` 是字面星号，`/**/` 也匹配 `/`；只有一个 `*` 且以 ` *` 结尾时，
/// 结尾部分可以整个省略（`git *` 也匹配 `git`）。比较前模式与命令里连续的空格、制表符并成一个空格。
///
/// 修复（工作线 F85）：以前不并空白，`Bash(cp *)` 拦不住 `cp⇥x y`。
pub fn wildcard_matches(pattern: &str, command: &str) -> bool {
    let pattern = squeeze(pattern.trim());
    let command = squeeze(command);
    let mut regex = String::from("^");
    let mut stars = 0;
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'*') => {
                chars.next();
                regex.push_str(r"\*");
            }
            '\\' if chars.peek() == Some(&'\\') => {
                chars.next();
                regex.push_str(r"\\");
            }
            '/' if chars.peek() == Some(&'*') => {
                // `/**/`（可以连着几段）匹配 `/` 或 `/…/`。
                let rest: String = chars.clone().collect();
                let mut globstars = 0;
                while rest[globstars * GLOBSTAR_SEGMENT.len()..].starts_with(GLOBSTAR_SEGMENT) {
                    globstars += 1;
                }
                if globstars > 0 {
                    for _ in 0..globstars * GLOBSTAR_SEGMENT.len() {
                        chars.next();
                    }
                    stars += globstars * 2;
                    regex.push_str("/(?:.*/)?");
                } else {
                    regex.push('/');
                }
            }
            '*' => {
                stars += 1;
                regex.push_str(".*");
            }
            other => {
                if ".+?^${}()|[]\\'\"".contains(other) {
                    regex.push('\\');
                }
                regex.push(other);
            }
        }
    }
    if stars == 1 && regex.ends_with(TRAILING_ANY) {
        regex.truncate(regex.len() - TRAILING_ANY.len());
        regex.push_str("( .*)?");
    }
    regex.push('$');
    Regex::new(&format!("(?s){regex}")).is_ok_and(|re| re.is_match(&command))
}

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
        let stripped = crate::shell_argv::strip_wrappers(argv);
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
    let rule = parse(content);
    candidates.iter().any(|command| match (rule, mode) {
        (ShellRule::Exact(exact), _) => exact == command,
        (ShellRule::Prefix(prefix), Mode::Exact) => squeeze(prefix) == squeeze(command),
        (ShellRule::Wildcard(_), Mode::Exact) => false,
        (ShellRule::Prefix(prefix), Mode::Prefix) => {
            let (prefix, command) = (squeeze(prefix), squeeze(command));
            let xargs = format!("xargs {prefix}");
            command == prefix
                || command.starts_with(&format!("{prefix} "))
                || command == xargs
                || command.starts_with(&format!("{xargs} "))
        }
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
