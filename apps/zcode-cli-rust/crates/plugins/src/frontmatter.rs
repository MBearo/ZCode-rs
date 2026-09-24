//! `name`/`description` from Markdown frontmatter, including YAML block
//! scalars (Node `markdown-frontmatter.ts`).
use crate::js::{is_space, trim};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Frontmatter {
    pub name: Option<String>,
    pub description: Option<String>,
}

/// Node `readMarkdownFrontmatter`: unreadable files have no frontmatter.
pub async fn read(path: &Path) -> Frontmatter {
    match crate::fsx::read_text(path).await {
        Ok(content) => parse(&content),
        Err(_) => Frontmatter::default(),
    }
}

fn split_lines(text: &str) -> Vec<&str> {
    text.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect()
}

fn starts_with_space(line: &str) -> bool {
    line.chars().next().is_some_and(is_space)
}

pub fn parse(content: &str) -> Frontmatter {
    let Some(block) = extract(content) else {
        return Frontmatter::default();
    };
    let values = scalars(&block);
    // Node 只在值非空时写入（`if (name)`）。
    let non_empty = |key: &str| scalar(values.get(key)).filter(|v| !v.is_empty());
    Frontmatter {
        name: non_empty("name"),
        description: non_empty("description"),
    }
}

fn extract(content: &str) -> Option<String> {
    let normalized = content.strip_prefix('\u{feff}').unwrap_or(content);
    if !normalized.starts_with("---") {
        return None;
    }
    let lines = split_lines(normalized);
    if trim(lines[0]) != "---" {
        return None;
    }
    let end = lines
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, line)| trim(line) == "---")?
        .0;
    Some(lines[1..end].join("\n"))
}

fn scalars(frontmatter: &str) -> HashMap<String, String> {
    let lines = split_lines(frontmatter);
    let mut values = HashMap::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        index += 1;
        if trim(line).is_empty() || trim(line).starts_with('#') || starts_with_space(line) {
            continue;
        }
        let Some(separator) = line.find(':').filter(|s| *s > 0) else {
            continue;
        };
        let key = trim(&line[..separator]).to_owned();
        let value = trim(&line[separator + 1..]);
        if values.contains_key(&key) {
            continue;
        }
        let folded = matches!(value, ">" | ">+" | ">-");
        if folded || matches!(value, "|" | "|+" | "|-") {
            let (block, next) = block_scalar(&lines, index, folded);
            values.insert(key, block);
            index = next;
        } else {
            values.insert(key, value.to_owned());
        }
    }
    values
}

fn block_scalar(lines: &[&str], start: usize, folded: bool) -> (String, usize) {
    let mut index = start;
    while index < lines.len() && (trim(lines[index]).is_empty() || starts_with_space(lines[index]))
    {
        index += 1;
    }
    let raw = &lines[start..index];
    let indent = raw
        .iter()
        .filter(|line| !trim(line).is_empty())
        .map(|line| line.chars().take_while(|c| is_space(*c)).count())
        .min();
    let content: Vec<String> = raw
        .iter()
        .map(|line| {
            if trim(line).is_empty() {
                String::new()
            } else {
                line.chars().skip(indent.unwrap_or(0)).collect()
            }
        })
        .collect();
    let value = if folded {
        let mut paragraphs = vec![];
        let mut current: Vec<&str> = vec![];
        for line in &content {
            let trimmed = trim(line);
            if trimmed.is_empty() {
                if !current.is_empty() {
                    paragraphs.push(current.join(" "));
                    current.clear();
                }
                continue;
            }
            current.push(trimmed);
        }
        if !current.is_empty() {
            paragraphs.push(current.join(" "));
        }
        trim(&paragraphs.join("\n")).to_owned()
    } else {
        trim(&content.join("\n")).to_owned()
    };
    (value, index)
}

fn scalar(value: Option<&String>) -> Option<String> {
    let trimmed = trim(value?);
    if trimmed.is_empty() {
        return None;
    }
    let quoted = (trimmed.starts_with('"') && trimmed.ends_with('"'))
        || (trimmed.starts_with('\'') && trimmed.ends_with('\''));
    // JS 的 slice(1, -1) 对单个引号返回空串。
    Some(match quoted {
        true if trimmed.len() < 2 => String::new(),
        true => trim(&trimmed[1..trimmed.len() - 1]).to_owned(),
        false => trimmed.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_scalars_and_quotes_follow_node() {
        let md = "\u{feff}---\r\nname: \"demo\"\ndescription: >\n  first line\n  continues\n\n  second\nother: x\n---\nbody";
        assert_eq!(
            parse(md),
            Frontmatter {
                name: Some("demo".into()),
                description: Some("first line continues\nsecond".into())
            }
        );
        let literal = "---\ndescription: |-\n    a\n      b\nname: n\n---";
        assert_eq!(parse(literal).description.as_deref(), Some("a\n  b"));
        assert_eq!(parse("---\nname: ''\n---").name, None);
        assert_eq!(parse("no frontmatter"), Frontmatter::default());
        assert_eq!(
            parse("---\nname: a\n"),
            Frontmatter::default(),
            "unterminated"
        );
        assert_eq!(
            parse("---\nname: a\nname: b\n---").name.as_deref(),
            Some("a")
        );
    }
}
