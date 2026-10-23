//! Bash 规则的匹配（shell-permissions.md §4.2）。
//!
//! content 有三种：`git status:*` 是前缀（前缀后必须是结尾或空格），含未转义 `*` 的是通配，其余
//! 精确相等。匹配前先去掉安全的环境变量前缀与包装命令（`timeout 10`、`nice`、`nohup` 等），
//! 前缀与通配规则不匹配复合命令（复合命令按子命令分别匹配）。

use std::sync::OnceLock;

use regex::Regex;

/// 一条 Bash 规则的 content。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellRule<'a> {
    Exact(&'a str),
    Prefix(&'a str),
    Wildcard(&'a str),
}

fn has_wildcard(pattern: &str) -> bool {
    if pattern.ends_with(":*") {
        return false;
    }
    pattern.char_indices().any(|(index, c)| {
        c == '*'
            && pattern[..index]
                .bytes()
                .rev()
                .take_while(|byte| *byte == b'\\')
                .count()
                % 2
                == 0
    })
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

/// 通配匹配：`*` 匹配任意文本，`\*` 是字面星号；只有一个 `*` 且以 ` *` 结尾时，
/// 结尾部分可以整个省略（`git *` 也匹配 `git`）。
pub fn wildcard_matches(pattern: &str, command: &str) -> bool {
    let pattern = pattern.trim();
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
    if stars == 1 && regex.ends_with(" .*") {
        regex.truncate(regex.len() - 3);
        regex.push_str("( .*)?");
    }
    regex.push('$');
    Regex::new(&format!("(?s){regex}")).is_ok_and(|re| re.is_match(command))
}

struct Patterns {
    env: Regex,
    wrappers: Vec<Regex>,
}

fn patterns() -> Option<&'static Patterns> {
    static PATTERNS: OnceLock<Option<Patterns>> = OnceLock::new();
    PATTERNS
        .get_or_init(|| {
            let wrappers = [
                r"^timeout[ \t]+(?:(?:--(?:foreground|preserve-status|verbose)|--(?:kill-after|signal)=[A-Za-z0-9_.+-]+|--(?:kill-after|signal)[ \t]+[A-Za-z0-9_.+-]+|-v|-[ks][ \t]+[A-Za-z0-9_.+-]+|-[ks][A-Za-z0-9_.+-]+)[ \t]+)*(?:--[ \t]+)?\d+(?:\.\d+)?[smhd]?[ \t]+",
                r"^time[ \t]+(?:--[ \t]+)?",
                r"^nice(?:[ \t]+-n[ \t]+-?\d+|[ \t]+-\d+)?[ \t]+(?:--[ \t]+)?",
                r"^stdbuf(?:[ \t]+-[ioe][LN0-9]+)+[ \t]+(?:--[ \t]+)?",
                r"^nohup[ \t]+(?:--[ \t]+)?",
            ]
            .iter()
            .map(|source| Regex::new(source).ok())
            .collect::<Option<Vec<_>>>()?;
            Some(Patterns {
                env: Regex::new(r"^([A-Za-z_][A-Za-z0-9_]*)=([A-Za-z0-9_./:-]+)[ \t]+").ok()?,
                wrappers,
            })
        })
        .as_ref()
}

/// 去掉整行注释（以 `#` 开头的行）；全是注释时原样返回。
fn strip_comment_lines(command: &str) -> String {
    let kept: Vec<&str> = command
        .lines()
        .filter(|line| {
            let line = line.trim();
            !line.is_empty() && !line.starts_with('#')
        })
        .collect();
    if kept.is_empty() {
        command.to_owned()
    } else {
        kept.join("\n")
    }
}

/// 去掉安全的前缀（环境变量与包装命令）。
pub fn strip_safe_wrappers(command: &str) -> String {
    let Some(patterns) = patterns() else {
        return command.trim().to_owned();
    };
    let mut stripped = command.to_owned();
    loop {
        let before = stripped.clone();
        stripped = strip_comment_lines(&stripped);
        if let Some(found) = patterns.env.captures(&stripped)
            && found
                .get(1)
                .is_some_and(|name| crate::safe_env::is_safe(name.as_str()))
        {
            stripped = stripped[found.get(0).map_or(0, |all| all.end())..].to_owned();
        }
        if stripped == before {
            break;
        }
    }
    loop {
        let before = stripped.clone();
        stripped = strip_comment_lines(&stripped);
        for pattern in &patterns.wrappers {
            stripped = pattern.replace(&stripped, "").into_owned();
        }
        if stripped == before {
            break;
        }
    }
    stripped.trim().to_owned()
}

/// 规则与命令怎样比较。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 只比完整命令。
    Exact,
    /// 前缀与通配（单个子命令）。
    Prefix,
}

/// 用来匹配的候选：原命令，以及去掉安全包装后的命令。
pub fn candidates(command: &str) -> Vec<String> {
    let command = command.trim().to_owned();
    let stripped = strip_safe_wrappers(&command);
    if stripped == command {
        vec![command]
    } else {
        vec![command, stripped]
    }
}

/// 一条 content 是否匹配候选之一。`compound` 判断候选是不是复合命令（前缀、通配不匹配它）。
pub fn matches(
    content: &str,
    candidates: &[String],
    mode: Mode,
    compound: impl Fn(&str) -> bool,
) -> bool {
    let rule = parse(content);
    candidates.iter().any(|command| match (rule, mode) {
        (ShellRule::Exact(exact), _) => exact == command,
        (ShellRule::Prefix(prefix), Mode::Exact) => prefix == command,
        (ShellRule::Wildcard(_), Mode::Exact) => false,
        (_, Mode::Prefix) if compound(command) => false,
        (ShellRule::Prefix(prefix), Mode::Prefix) => {
            let xargs = format!("xargs {prefix}");
            command == prefix
                || command.starts_with(&format!("{prefix} "))
                || *command == xargs
                || command.starts_with(&format!("{xargs} "))
        }
        (ShellRule::Wildcard(pattern), Mode::Prefix) => wildcard_matches(pattern, command),
    })
}

#[cfg(test)]
#[path = "shell_rules_tests.rs"]
mod tests;
