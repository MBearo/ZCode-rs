//! 规则匹配前去掉命令开头的东西（shell-permissions.md §4.2、§4.5.6）：整行注释、前缀赋值、包装命令、命令名的引号。
//!
//! 两个函数，出处见 provenance/policy.md 的 F85：
//! - [`strip_safe_wrappers`]：只去安全名单里的前缀赋值，再去命令名的引号与 `timeout`、`time`、`nice`、`stdbuf`、
//!   `nohup`、`command`、`builtin`、`noglob`。allow、deny、ask 规则都用它。
//! - [`strip_all_assignments`]：去掉开头的全部前缀赋值（值里有展开的除外）。只给 deny 与 ask 规则用：allow 规则只
//!   认安全名单，`DOCKER_HOST=… docker ps` 不能借 `Bash(docker ps:*)` 放行。

use std::sync::OnceLock;

use regex::Regex;

struct Patterns {
    safe_env: Regex,
    any_env: Regex,
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
                safe_env: Regex::new(r"^([A-Za-z_][A-Za-z0-9_]*)=([A-Za-z0-9_./:-]+)[ \t]+").ok()?,
                // 值：单引号串、双引号串（没有展开与反引号）、反斜杠转义、其余安全字符；后面要有空格或制表符。
                any_env: Regex::new(
                    r#"^([A-Za-z_][A-Za-z0-9_]*(?:\[[^\]]*\])?)\+?=(?:'[^'\n\r]*'|"(?:\\[^\n\r\x{2028}\x{2029}]|[^"$`\\\n\r])*"|\\[^\n\r\x{2028}\x{2029}]|[^ \t\n\r$`;|&()<>\\'"])*[ \t]+"#,
                )
                .ok()?,
                wrappers,
            })
        })
        .as_ref()
}

/// 去掉整行注释（去掉首尾空白后以 `#` 开头的行）；全是注释时原样返回。空行留着。
pub(crate) fn strip_comment_lines(command: &str) -> String {
    let kept: Vec<&str> = command
        .split('\n')
        .filter(|line| !line.trim().starts_with('#'))
        .collect();
    if kept.is_empty() {
        command.to_owned()
    } else {
        kept.join("\n")
    }
}

/// 第一个词去引号：反斜杠转义、单引号、双引号（双引号里只认 `\$`、`` \` ``、`\"`、`\\`）。引号没闭合或以反斜杠
/// 结尾时原样返回。
fn unquote_first_word(command: &str) -> String {
    if command.chars().next().is_none_or(char::is_whitespace) {
        return command.to_owned();
    }
    let end = command
        .char_indices()
        .find(|(_, c)| c.is_whitespace())
        .map_or(command.len(), |(index, _)| index);
    let (word, rest) = command.split_at(end);
    let chars: Vec<char> = word.chars().collect();
    let mut out = String::with_capacity(word.len());
    let mut quote: Option<char> = None;
    let mut dangling = false;
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        dangling = false;
        match quote {
            Some('\'') => {
                if c == '\'' {
                    quote = None;
                } else {
                    out.push(c);
                }
            }
            Some(_) => {
                if c == '\\' {
                    match chars.get(index + 1) {
                        Some(&next @ ('$' | '`' | '"' | '\\')) => {
                            out.push(next);
                            index += 1;
                        }
                        None => dangling = true,
                        Some(_) => out.push(c),
                    }
                } else if c == '"' {
                    quote = None;
                } else {
                    out.push(c);
                }
            }
            None => {
                if c == '\\' {
                    match chars.get(index + 1) {
                        None => dangling = true,
                        Some(&next) => {
                            out.push(next);
                            index += 1;
                        }
                    }
                } else if c == '"' || c == '\'' {
                    quote = Some(c);
                } else {
                    out.push(c);
                }
            }
        }
        index += 1;
    }
    if quote.is_some() || dangling {
        return command.to_owned();
    }
    out.push_str(rest);
    out
}

fn blank_run(text: &str) -> usize {
    text.bytes()
        .take_while(|b| matches!(b, b' ' | b'\t'))
        .count()
}

/// `[ \t]+(?!-)` 接在 `at` 之后：返回匹配的结尾。JS 的回溯让两个以上空白时总能匹配（少吃一个空白，后面是空白）。
fn blanks_not_before_dash(text: &str, at: usize) -> Option<usize> {
    let run = blank_run(&text[at..]);
    if run == 0 {
        return None;
    }
    if !text[at + run..].starts_with('-') {
        Some(at + run)
    } else if run >= 2 {
        Some(at + run - 1)
    } else {
        None
    }
}

/// `[ \t]+--` 接在 `at` 之后。
fn blanks_dashes(text: &str, at: usize) -> Option<usize> {
    let run = blank_run(&text[at..]);
    (run > 0 && text[at + run..].starts_with("--")).then_some(at + run + 2)
}

/// `^command(?:[ \t]+-p+)*(?:[ \t]+--)?[ \t]+(?!-)`、`^builtin(?:[ \t]+--)?[ \t]+(?!-)`、`^noglob[ \t]+(?!-)`
/// 的匹配长度（按 JS 回溯的先后：`-p` 组尽量多、`--` 先取）。
fn lookahead_wrapper(text: &str) -> Option<usize> {
    let (name, options) = ["command", "builtin", "noglob"]
        .into_iter()
        .zip([true, false, false])
        .find(|(name, _)| text.starts_with(name))?;
    let mut ends = vec![name.len()];
    if options {
        let mut at = name.len();
        loop {
            let run = blank_run(&text[at..]);
            let token = at + run;
            if run == 0 || !text[token..].starts_with('-') {
                break;
            }
            let ps = text[token + 1..].bytes().take_while(|b| *b == b'p').count();
            if ps == 0 {
                break;
            }
            at = token + 1 + ps;
            ends.push(at);
        }
    }
    let dashes = name != "noglob";
    ends.into_iter().rev().find_map(|at| {
        dashes
            .then(|| blanks_dashes(text, at).and_then(|end| blanks_not_before_dash(text, end)))
            .flatten()
            .or_else(|| blanks_not_before_dash(text, at))
    })
}

/// 去掉一个包装命令；没有时 None。
fn strip_wrapper_once(text: &str, patterns: &Patterns) -> String {
    let mut stripped = text.to_owned();
    for pattern in &patterns.wrappers {
        stripped = pattern.replace(&stripped, "").into_owned();
    }
    if let Some(end) = lookahead_wrapper(&stripped) {
        stripped = stripped[end..].to_owned();
    }
    stripped
}

/// 去掉安全的前缀（环境变量、命令名的引号与包装命令）。
///
/// 修复（工作线 F85）：以前没有 `command`、`builtin`、`noglob` 与命令名的引号，`\rm`、`"rm"`、`command rm` 都比不上
/// `Bash(rm:*)`；整行注释也去掉空行，与对照实现不同。
pub fn strip_safe_wrappers(command: &str) -> String {
    let Some(patterns) = patterns() else {
        return command.trim().to_owned();
    };
    let mut stripped = command.to_owned();
    loop {
        let before = stripped.clone();
        stripped = strip_comment_lines(&stripped);
        if let Some(found) = patterns.safe_env.captures(&stripped)
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
    stripped = unquote_first_word(&strip_comment_lines(&stripped));
    loop {
        let before = stripped.clone();
        stripped = strip_wrapper_once(&stripped, patterns);
        if stripped == before {
            break;
        }
        stripped = unquote_first_word(&strip_comment_lines(&stripped));
    }
    stripped.trim().to_owned()
}

/// 去掉开头的全部前缀赋值（deny 与 ask 规则用，§4.5.6）。值里有展开的不去。
pub(crate) fn strip_all_assignments(command: &str) -> String {
    let Some(patterns) = patterns() else {
        return command.trim().to_owned();
    };
    let mut stripped = command.to_owned();
    loop {
        let before = stripped.clone();
        stripped = strip_comment_lines(&stripped);
        if let Some(found) = patterns.any_env.find(&stripped) {
            stripped = stripped[found.end()..].to_owned();
        }
        if stripped == before {
            break;
        }
    }
    stripped.trim().to_owned()
}

#[cfg(test)]
#[path = "shell_strip_tests.rs"]
mod tests;
