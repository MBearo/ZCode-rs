//! gitignore 语义的路径匹配（一组模式，判断相对路径是否被它们忽略；语义同 npm 的 `ignore` 包）。
//! 条件规则的 `paths`（context.md §11.4）与 skill 的 `paths`
//! （ext.md §2.6）都用它（F58：以前 context 与 ext 各有一份逐字相同的副本）。
//!
//! - 不带 `/` 的模式在任意层级匹配名字；带 `/`（开头或中间）的从根匹配；
//! - `*`、`?` 不跨目录，`**` 跨目录，`[...]` 是字符类，`\` 转义；
//! - 结尾的 `/` 只匹配目录；`!` 取反；
//! - 先查每一层父目录（`a/`、`a/b/`），父目录命中时其下一切都算命中；
//! - 不分大小写（`ignore` 包默认 `ignorecase: true`）。

use regex::Regex;

struct Rule {
    negated: bool,
    regex: Regex,
}

pub struct Globs(Vec<Rule>);

/// 一段路径（不含 `/`）里的通配符换成正则。
fn segment(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => {
                while chars.peek() == Some(&'*') {
                    chars.next();
                }
                out.push_str("[^/]*");
            }
            '?' => out.push_str("[^/]"),
            '\\' => match chars.next() {
                Some(escaped) => out.push_str(&regex::escape(&escaped.to_string())),
                None => out.push_str(r"\\"),
            },
            '[' => {
                let mut class = String::from("[");
                if chars.peek() == Some(&'!') {
                    chars.next();
                    class.push('^');
                }
                let mut closed = false;
                for inner in chars.by_ref() {
                    if inner == ']' {
                        closed = true;
                        break;
                    }
                    if inner == '\\' || inner == '[' {
                        class.push('\\');
                    }
                    class.push(inner);
                }
                if closed {
                    out.push_str(&class);
                    out.push(']');
                } else {
                    out.push_str(r"\[");
                    out.push_str(&regex::escape(&class[1..]));
                }
            }
            other => out.push_str(&regex::escape(&other.to_string())),
        }
    }
    out
}

fn compile(pattern: &str) -> Option<Rule> {
    let pattern = pattern.trim_end_matches(' ');
    if pattern.is_empty() || pattern.starts_with('#') {
        return None;
    }
    let (negated, pattern) = match pattern.strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, pattern.strip_prefix('\\').unwrap_or(pattern)),
    };
    let (dir_only, pattern) = match pattern.strip_suffix('/') {
        Some(rest) => (true, rest),
        None => (false, pattern),
    };
    let anchored = pattern.contains('/');
    let pattern = pattern.strip_prefix('/').unwrap_or(pattern);
    if pattern.is_empty() {
        return None;
    }
    let parts: Vec<&str> = pattern.split('/').collect();
    let mut body = String::new();
    for (index, part) in parts.iter().enumerate() {
        let last = index + 1 == parts.len();
        if *part == "**" {
            // `**/` 是零或多层目录；结尾的 `**` 是其下的一切。
            body.push_str(if last { ".*" } else { "(?:[^/]*/)*" });
            continue;
        }
        body.push_str(&segment(part));
        if !last {
            body.push('/');
        }
    }
    let prefix = if anchored { "^" } else { "(?:^|/)" };
    let suffix = if dir_only { "/$" } else { "/?$" };
    let regex = Regex::new(&format!("(?i){prefix}{body}{suffix}")).ok()?;
    Some(Rule { negated, regex })
}

impl Globs {
    /// 无法编译的模式先过滤掉，其余照常匹配。
    pub fn new(patterns: &[String]) -> Self {
        Self(
            patterns
                .iter()
                .filter_map(|pattern| compile(pattern))
                .collect(),
        )
    }

    /// `ignore` 包的 `_testOne`：没被忽略时只看正向规则，已被忽略时只看取反规则。
    fn test_one(&self, path: &str) -> bool {
        let mut ignored = false;
        for rule in &self.0 {
            if rule.negated != ignored {
                continue;
            }
            if rule.regex.is_match(path) {
                ignored = !rule.negated;
            }
        }
        ignored
    }

    /// `relative` 用 `/` 分隔，不以 `/`、`./`、`../` 开头。
    pub fn matches(&self, relative: &str) -> bool {
        let parts: Vec<&str> = relative.split('/').collect();
        for depth in 1..parts.len() {
            if self.test_one(&format!("{}/", parts[..depth].join("/"))) {
                return true;
            }
        }
        self.test_one(relative)
    }
}

#[cfg(test)]
#[path = "glob_tests.rs"]
mod tests;
