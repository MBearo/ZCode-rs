//! `--input-format stream-json` 的一行（docs/specs/rust-cli/headless.md §4）：SDK 的输入格式，
//! 只认文字的 user 消息。

use serde_json::Value;

/// 一行的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Line {
    /// 空行、不是 user 的消息（`assistant`、`control_request` 等）：忽略。
    Skip,
    /// 一条 user 消息的文字：开一个回合。
    Prompt(String),
}

/// 解析一行；错误是写到 stderr 的原文（退出码 1）。
pub(crate) fn parse(line: &str) -> Result<Line, String> {
    if line.trim().is_empty() {
        return Ok(Line::Skip);
    }
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        // `Error parsing streaming input line (type=unknown, <长度> chars): SyntaxError`，长度是 JS 字符串长度。
        return Err(format!(
            "Error parsing streaming input line (type=unknown, {} chars): SyntaxError",
            line.encode_utf16().count()
        ));
    };
    if value.get("type").and_then(Value::as_str) != Some("user") {
        return Ok(Line::Skip);
    }
    let message = value.get("message");
    let role = message.and_then(|message| message.get("role"));
    if role.and_then(Value::as_str) != Some("user") {
        let role = match role {
            None | Some(Value::Null) => "undefined".to_owned(),
            Some(Value::String(text)) => text.clone(),
            Some(other) => other.to_string(),
        };
        return Err(format!("Error: Expected message role 'user', got '{role}'"));
    }
    let text = match message.and_then(|message| message.get("content")) {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    };
    Ok(Line::Prompt(text))
}

#[cfg(test)]
#[path = "input_tests.rs"]
mod tests;
