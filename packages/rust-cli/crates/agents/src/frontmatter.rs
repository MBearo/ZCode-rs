//! agent 文件的 frontmatter（agent 定义用到的键）。
//!
//! 不做完整的 YAML 解析；这里只支持 agent 文件常用的写法：`key: 值`（可加引号）、`key: [a, b]`、
//! 下一行起的 `- 项` 列表、`|` / `>` 块文本（agents.md §3）。不认识的写法按缺省处理。

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Value {
    Text(String),
    List(Vec<String>),
}

impl Value {
    pub(crate) fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::List(_) => None,
        }
    }
}

/// `---` 包住的 frontmatter 与之后的正文；没有 frontmatter 时为 None。
pub(crate) fn split(raw: &str) -> Option<(BTreeMap<String, Value>, String)> {
    let text = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let rest = text.strip_prefix("---")?;
    let rest = rest.trim_start_matches([' ', '\t']);
    let rest = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))?;
    let (yaml, body) = closing(rest)?;
    Some((parse(yaml), body.to_owned()))
}

/// 找到单独成行的 `---`：返回之前的 frontmatter 与之后的正文。
fn closing(rest: &str) -> Option<(&str, &str)> {
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            return Some((&rest[..offset], &rest[offset + line.len()..]));
        }
        offset += line.len();
    }
    (rest[offset..].trim_end() == "---").then(|| (&rest[..offset], ""))
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    let quoted =
        |quote: char| value.len() >= 2 && value.starts_with(quote) && value.ends_with(quote);
    if quoted('"') || quoted('\'') {
        value[1..value.len() - 1].to_owned()
    } else {
        value.to_owned()
    }
}

fn inline_list(value: &str) -> Option<Vec<String>> {
    let inner = value.trim().strip_prefix('[')?.strip_suffix(']')?;
    Some(
        inner
            .split(',')
            .map(unquote)
            .filter(|item| !item.is_empty())
            .collect(),
    )
}

fn parse(yaml: &str) -> BTreeMap<String, Value> {
    let mut map = BTreeMap::new();
    let lines: Vec<&str> = yaml.lines().collect();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        index += 1;
        if line.trim().is_empty()
            || line.trim_start().starts_with('#')
            || line.starts_with([' ', '\t'])
        {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim().to_owned();
        let value = value.trim();
        // 缩进的后续行：列表项或块文本。
        let mut nested = Vec::new();
        while index < lines.len()
            && (lines[index].starts_with([' ', '\t']) || lines[index].trim().is_empty())
        {
            nested.push(lines[index]);
            index += 1;
        }
        let parsed = if let Some(list) = inline_list(value) {
            Value::List(list)
        } else if value == "|" || value == ">" {
            let body: Vec<&str> = nested.iter().map(|line| line.trim()).collect();
            let joiner = if value == "|" { "\n" } else { " " };
            Value::Text(body.join(joiner).trim().to_owned())
        } else if value.is_empty() {
            let items: Vec<String> = nested
                .iter()
                .filter_map(|line| line.trim().strip_prefix('-'))
                .map(unquote)
                .collect();
            if items.is_empty() {
                continue;
            }
            Value::List(items)
        } else {
            Value::Text(unquote(value))
        };
        map.insert(key, parsed);
    }
    map
}

/// 工具清单：缺省为 None（全部）；含 `*` 也是全部；逗号分隔或列表。
pub(crate) fn tools(value: Option<&Value>) -> Option<Vec<String>> {
    let items: Vec<String> = match value? {
        Value::Text(text) => text
            .split(',')
            .map(|item| item.trim().to_owned())
            .filter(|item| !item.is_empty())
            .collect(),
        Value::List(items) => items.clone(),
    };
    (!items.iter().any(|item| item == "*")).then_some(items)
}

#[cfg(test)]
#[path = "frontmatter_tests.rs"]
mod tests;
