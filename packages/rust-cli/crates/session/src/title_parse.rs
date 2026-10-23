//! 标题模型回复的整理（docs/specs/rust-cli/titles.md §3）：Node `normalizeTitleInput` 与
//! `cleanGeneratedTitle`（`apps/zcode-cli/packages/core/src/runtime/methods/title-generation-sidecar.ts`）。

// 压空白只定义在 base（F58：以前这里写了一份 `collapse`）。
use rust_cli_base::js::string::{collapse_spaces as collapse, is_space, trim, utf16_prefix};
use serde_json::Value;

/// Node `MAX_TITLE_INPUT_CHARS`（UTF-16 码元）。
const MAX_TITLE_INPUT_CHARS: usize = 1_200;
/// Node `MAX_TITLE_CHARS`（UTF-16 码元）。
const MAX_TITLE_CHARS: usize = 100;

/// Node `normalizeTitleInput`：去首尾空白、连续空白换成一个空格、超过 1200 个 UTF-16 码元时截断。
pub(crate) fn normalize_input(input: &str) -> String {
    let normalized = collapse(trim(input));
    if normalized.encode_utf16().count() > MAX_TITLE_INPUT_CHARS {
        utf16_prefix(&normalized, MAX_TITLE_INPUT_CHARS).to_owned()
    } else {
        normalized
    }
}

/// 去掉 `<think>…</think>`（不分大小写，非贪婪；没有闭合标签时不动）。
fn without_thinking(raw: &str) -> String {
    let mut out = String::new();
    let mut rest = raw;
    loop {
        let lower = rest.to_ascii_lowercase();
        let Some(open) = lower.find("<think>") else {
            out.push_str(rest);
            break;
        };
        let Some(close) = lower[open..].find("</think>") else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..open]);
        rest = &rest[open + close + "</think>".len()..];
    }
    trim(&out).to_owned()
}

/// `` ^```[ \t]*(?:json)?[ \t]*\r?\n([\s\S]*?)\r?\n?```$ ``（不分大小写）的内容，去首尾空白。
fn fenced_json(text: &str) -> Option<String> {
    let text = trim(text);
    let after = text.strip_prefix("```")?;
    let after = after.trim_start_matches([' ', '\t']);
    let after = match after.get(..4) {
        Some(tag) if tag.eq_ignore_ascii_case("json") => &after[4..],
        _ => after,
    };
    let after = after.trim_start_matches([' ', '\t']);
    let body = after
        .strip_prefix("\r\n")
        .or_else(|| after.strip_prefix('\n'))?;
    let inner = body.strip_suffix("```")?;
    Some(trim(inner).to_owned())
}

fn title_json(text: &str) -> Option<String> {
    let value: Value = serde_json::from_str(text).ok()?;
    value.as_object()?.get("title")?.as_str().map(str::to_owned)
}

fn quote(c: char) -> bool {
    is_space(c) || matches!(c, '"' | '\'' | '`' | '“' | '”' | '‘' | '’')
}

/// Node `cleanGeneratedTitle`：没有可用标题时为 None。
pub(crate) fn clean_title(raw: &str) -> Option<String> {
    let text = without_thinking(raw);
    let parsed =
        title_json(&text).or_else(|| fenced_json(&text).and_then(|inner| title_json(&inner)));
    let candidate = parsed.or_else(|| {
        text.split('\n')
            .map(|line| trim(line.strip_suffix('\r').unwrap_or(line)))
            .find(|line| !line.is_empty())
            .map(str::to_owned)
    })?;
    let heading = candidate.trim_start_matches('#');
    let heading = if heading.len() < candidate.len() {
        heading.trim_start_matches(is_space)
    } else {
        &candidate
    };
    let unquoted = heading.trim_matches(quote);
    let unpunctuated = unquoted.trim_end_matches([
        '.', '。', '!', '！', '?', '？', ':', '：', ',', '，', ';', '；',
    ]);
    let cleaned = trim(&collapse(unpunctuated)).to_owned();
    let meaningful = cleaned
        .chars()
        .any(|c| c.is_ascii_alphanumeric() || ('\u{3400}'..='\u{9fff}').contains(&c));
    if !meaningful {
        return None;
    }
    if cleaned.encode_utf16().count() > MAX_TITLE_CHARS {
        let head = trim(utf16_prefix(&cleaned, MAX_TITLE_CHARS - 3));
        return Some(format!("{head}..."));
    }
    Some(cleaned)
}

#[cfg(test)]
#[path = "title_parse_tests.rs"]
mod tests;
