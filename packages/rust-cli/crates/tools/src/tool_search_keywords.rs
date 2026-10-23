//! ToolSearch 的关键词查询（docs/specs/rust-cli/tools-more.md §3）。

use std::collections::HashMap;

use regex::Regex;

/// 一个候选的延迟工具。
pub(crate) struct Candidate {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) hint: Option<String>,
    pub(crate) mcp: bool,
}

/// `mcp__<server>__<tool>` 里的 tool 名。
pub(crate) fn mcp_tool_name(name: &str) -> Option<&str> {
    let rest = name.strip_prefix("mcp__")?;
    let (_, tool) = rest.split_once("__")?;
    (!tool.is_empty()).then_some(tool)
}

fn mcp_parts(name: &str) -> Option<(String, Option<String>)> {
    let rest = name.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__").unwrap_or((rest, ""));
    Some((
        server.to_owned(),
        (!tool.is_empty()).then(|| tool.to_owned()),
    ))
}

/// 名字拆词：MCP 按 server 与 tool 拆，其余按 CamelCase 与 `_` 拆。
struct NameParts {
    parts: Vec<String>,
    coarse: Vec<String>,
    full: String,
    mcp: bool,
}

fn name_parts(candidate: &Candidate) -> NameParts {
    let info = mcp_parts(&candidate.name).or_else(|| {
        candidate
            .mcp
            .then(|| (candidate.name.clone(), None::<String>))
    });
    if let Some((server, tool)) = info {
        let coarse: Vec<String> = [Some(server), tool]
            .into_iter()
            .flatten()
            .filter(|part| !part.is_empty())
            .map(|part| part.to_lowercase())
            .collect();
        let parts: Vec<String> = coarse
            .iter()
            .flat_map(|part| {
                part.split(|ch: char| ch.is_whitespace() || ch == '_' || ch == '.')
                    .filter(|piece| !piece.is_empty())
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .collect();
        return NameParts {
            full: parts.join(" "),
            parts,
            coarse,
            mcp: true,
        };
    }
    let mut spaced = String::new();
    let mut previous: Option<char> = None;
    for ch in candidate.name.chars() {
        if ch.is_ascii_uppercase() && previous.is_some_and(|p| p.is_ascii_lowercase()) {
            spaced.push(' ');
        }
        spaced.push(if ch == '_' { ' ' } else { ch });
        previous = Some(ch);
    }
    let parts: Vec<String> = spaced
        .to_lowercase()
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    NameParts {
        full: parts.join(" "),
        parts,
        coarse: vec![candidate.name.to_lowercase()],
        mcp: false,
    }
}

/// 词边界匹配（JavaScript 的 `\b` 只认 ASCII 单词字符）。
fn word(term: &str) -> Option<Regex> {
    Regex::new(&format!(r"(?-u:\b){}(?-u:\b)", regex::escape(term))).ok()
}

fn matches(pattern: Option<&Regex>, text: &str) -> bool {
    pattern.is_some_and(|pattern| pattern.is_match(text))
}

/// 返回最多 `max` 个工具名，按得分降序（同分保持候选顺序）。`all` 是全部工具名（完全同名时的回退）。
pub(crate) fn search(
    query: &str,
    deferred: &[Candidate],
    all: &[String],
    max: usize,
) -> Vec<String> {
    let query = query.trim().to_lowercase();
    if let Some(exact) = deferred
        .iter()
        .map(|candidate| &candidate.name)
        .chain(all)
        .find(|name| name.to_lowercase() == query)
    {
        return vec![exact.clone()];
    }
    if query.starts_with("mcp__") && query.len() > 5 {
        let prefixed: Vec<String> = deferred
            .iter()
            .filter(|candidate| candidate.name.to_lowercase().starts_with(&query))
            .take(max)
            .map(|candidate| candidate.name.clone())
            .collect();
        if !prefixed.is_empty() {
            return prefixed;
        }
    }
    let words: Vec<&str> = query.split_whitespace().collect();
    let (required, optional): (Vec<&str>, Vec<&str>) = words
        .iter()
        .partition(|word| word.starts_with('+') && word.len() > 1);
    let required: Vec<&str> = required.iter().map(|word| &word[1..]).collect();
    let terms: Vec<&str> = if required.is_empty() {
        words.clone()
    } else {
        required.iter().chain(&optional).copied().collect()
    };
    let patterns: HashMap<&str, Option<Regex>> =
        terms.iter().map(|term| (*term, word(term))).collect();
    let mut scored: Vec<(String, u32)> = Vec::new();
    for candidate in deferred {
        let parts = name_parts(candidate);
        let description = candidate.description.to_lowercase();
        let hint = candidate.hint.as_deref().map(str::to_lowercase);
        let hint = hint.as_deref().filter(|hint| !hint.is_empty());
        let keep = required.iter().all(|term| {
            let pattern = patterns.get(term).and_then(Option::as_ref);
            parts.parts.iter().any(|part| part.contains(term))
                || parts.coarse.iter().any(|part| part.contains(term))
                || matches(pattern, &description)
                || hint.is_some_and(|hint| matches(pattern, hint))
        });
        if !keep {
            continue;
        }
        let mut score = 0;
        for term in &terms {
            let pattern = patterns.get(term).and_then(Option::as_ref);
            let (exact, partial, coarse_exact, coarse_partial) = if parts.mcp {
                (12, 6, 12, 4)
            } else {
                (10, 5, 10, 3)
            };
            if parts.parts.iter().any(|part| part == term) {
                score += exact;
            } else if parts.parts.iter().any(|part| part.contains(term)) {
                score += partial;
            }
            if parts.coarse.iter().any(|part| part == term) {
                score += coarse_exact;
            } else if parts.coarse.iter().any(|part| part.contains(term)) {
                score += coarse_partial;
            }
            if parts.full.contains(term) && score == 0 {
                score += 3;
            }
            if hint.is_some_and(|hint| matches(pattern, hint)) {
                score += 4;
            }
            if matches(pattern, &description) {
                score += 2;
            }
        }
        if score > 0 {
            scored.push((candidate.name.clone(), score));
        }
    }
    // 稳定排序：同分保持候选（按名字排好的延迟工具）的顺序，与 JavaScript 的 `sort` 相同。
    scored.sort_by_key(|(_, score)| std::cmp::Reverse(*score));
    scored.into_iter().take(max).map(|(name, _)| name).collect()
}

#[cfg(test)]
#[path = "tool_search_keywords_tests.rs"]
mod tests;
