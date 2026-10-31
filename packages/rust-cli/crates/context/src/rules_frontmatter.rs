//! 规则文件的 frontmatter：只取条件规则的 `paths`（context.md §11.4）。
//!
//! 步骤：匹配 frontmatter、逗号切分、花括号展开、引号回退、整理成 glob。不做完整的 YAML 解析，
//! 只解析 `paths` 一个键（差异见 context.md §7）。

use std::sync::OnceLock;

use regex::Regex;

/// frontmatter 的分隔行。
const FENCE: &str = "---";

/// frontmatter 的前缀：`^---\s*\n([\s\S]*?)---\s*\n?`。
fn pattern() -> Option<&'static Regex> {
    static PATTERN: OnceLock<Option<Regex>> = OnceLock::new();
    PATTERN
        .get_or_init(|| Regex::new(r"\A---\s*\n((?s:.*?))---\s*\n?").ok())
        .as_ref()
}

/// frontmatter 之后的正文，与 `paths` 给出的 glob（None 表示无条件规则）。
pub(crate) fn split(raw: &str) -> (String, Option<Vec<String>>) {
    let text = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    // 先看开头的 `---` 之后还有没有 `---`，没有就不匹配。
    let found = text
        .get(FENCE.len()..)
        .filter(|rest| rest.contains(FENCE))
        .and(pattern())
        .and_then(|pattern| pattern.captures(text));
    let Some(found) = found else {
        return (raw.to_owned(), None);
    };
    let (Some(whole), Some(yaml)) = (found.get(0), found.get(1)) else {
        return (raw.to_owned(), None);
    };
    let content = text[whole.end()..].to_owned();
    (content, paths(yaml.as_str()).and_then(globs))
}

enum Value {
    Text(String),
    List(Vec<String>),
}

fn unquote(value: &str) -> Option<String> {
    if let Some(inner) = value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        return Some(inner.replace("\\\"", "\"").replace("\\\\", "\\"));
    }
    value
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
        .map(|inner| inner.replace("''", "'"))
}

/// 无引号的标量：去掉 ` #` 起的注释。
fn plain(value: &str) -> String {
    let value = value.find(" #").map_or(value, |at| &value[..at]);
    value.trim().to_owned()
}

/// YAML 解析会失败、且引号回退救不回来的块列表项（别名 `*` 与保留字符开头）。
fn breaks_yaml(item: &str) -> bool {
    item.starts_with(['*', '@', '`'])
}

fn flow_items(inner: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut current = String::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for c in inner.chars() {
        match (quote, c) {
            (Some(open), c) if c == open => {
                quote = None;
                current.push(c);
            }
            (Some(_), c) => current.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                current.push(c);
            }
            (None, '{') => {
                depth += 1;
                current.push(c);
            }
            (None, '}') => {
                depth -= 1;
                current.push(c);
            }
            (None, ',') if depth == 0 => items.push(std::mem::take(&mut current)),
            (None, c) => current.push(c),
        }
    }
    items.push(current);
    items
        .iter()
        .map(|item| item.trim())
        .filter(|item| !item.is_empty())
        .map(|item| unquote(item).unwrap_or_else(|| plain(item)))
        .collect()
}

/// 顶层 `paths:` 的值。块列表项以别名等开头时 YAML 解析失败，frontmatter 为空。
fn paths(yaml: &str) -> Option<Value> {
    let lines: Vec<&str> = yaml.lines().collect();
    let at = lines
        .iter()
        .position(|line| line.strip_prefix("paths:").is_some())?;
    let value = lines[at].strip_prefix("paths:")?.trim();
    if value.is_empty() {
        let mut items = Vec::new();
        for line in &lines[at + 1..] {
            let Some(item) = line.trim_start().strip_prefix('-') else {
                break;
            };
            let item = item.trim();
            if breaks_yaml(item) {
                return None;
            }
            items.push(unquote(item).unwrap_or_else(|| plain(item)));
        }
        return (!items.is_empty()).then_some(Value::List(items));
    }
    if let Some(text) = unquote(value) {
        return Some(Value::Text(text));
    }
    if let Some(inner) = value.strip_prefix('[').and_then(|v| v.strip_suffix(']'))
        && !inner.trim_start().starts_with('*')
    {
        return Some(Value::List(flow_items(inner)));
    }
    // 以 YAML 指示符开头的值（`**/*.ts` 等）解析失败后，给整行加引号重试，值即原文。
    if value.starts_with(['*', '&', '!', '{', '[', '|', '>', '%', '@', '`']) {
        return Some(Value::Text(value.to_owned()));
    }
    Some(Value::Text(plain(value)))
}

/// 顶层逗号切分（花括号里的不切），再展开花括号。
fn split_patterns(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth = 0i32;
    for c in text.chars() {
        match c {
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    parts.push(current);
    parts
        .iter()
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .flat_map(expand_braces)
        .collect()
}

/// 展开第一个 `{a,b}`，再递归展开后面的。
fn expand_braces(pattern: &str) -> Vec<String> {
    let Some(open) = pattern.find('{') else {
        return vec![pattern.to_owned()];
    };
    let Some(close) = pattern[open..].find('}').map(|at| open + at) else {
        return vec![pattern.to_owned()];
    };
    if close == open + 1 {
        return vec![pattern.to_owned()];
    }
    let (prefix, alternatives, suffix) = (
        &pattern[..open],
        &pattern[open + 1..close],
        &pattern[close + 1..],
    );
    alternatives
        .split(',')
        .flat_map(|alternative| expand_braces(&format!("{prefix}{}{suffix}", alternative.trim())))
        .collect()
}

/// 整理成 glob：去掉结尾的 `/**`，去掉空项；没有项或全是 `**` 时为无条件规则。
fn globs(value: Value) -> Option<Vec<String>> {
    let patterns: Vec<String> = match value {
        Value::Text(text) => split_patterns(&text),
        Value::List(items) => items.iter().flat_map(|item| split_patterns(item)).collect(),
    }
    .into_iter()
    .map(|pattern| match pattern.strip_suffix("/**") {
        Some(stripped) => stripped.to_owned(),
        None => pattern,
    })
    .filter(|pattern| !pattern.is_empty())
    .collect();
    if patterns.is_empty() || patterns.iter().all(|pattern| pattern == "**") {
        return None;
    }
    Some(patterns)
}

#[cfg(test)]
#[path = "rules_frontmatter_tests.rs"]
mod tests;
