//! Rust canonical assistant messages to Node parts: the inverse of the cold
//! history mapping in `node_history::entries` (reasoning blocks per provider,
//! tool call inputs as objects).
use serde_json::{Value, json};

/// The reasoning parts of a canonical assistant message: `(text, providerOptions)`.
/// Anthropic thinking blocks and Responses reasoning items keep their provider
/// facts; plain `reasoning_content` becomes one part without metadata.
pub fn reasoning_parts(message: &Value) -> Vec<(String, Option<Value>)> {
    if let Some(blocks) = message["_zcode_anthropic_thinking"]
        .as_array()
        .filter(|b| !b.is_empty())
    {
        return blocks
            .iter()
            .filter_map(|block| match block["type"].as_str() {
                Some("thinking") => Some((
                    block["thinking"].as_str().unwrap_or("").to_owned(),
                    Some(json!({"anthropic": {"signature": block["signature"]}})),
                )),
                Some("redacted_thinking") => Some((
                    String::new(),
                    Some(json!({"anthropic": {"redactedData": block["data"]}})),
                )),
                _ => None,
            })
            .collect();
    }
    if let Some(items) = message["_zcode_responses_reasoning"]
        .as_array()
        .filter(|i| !i.is_empty())
    {
        let mut parts = vec![];
        for item in items {
            let options = json!({"openai": {"itemId": item["id"],
                "reasoningEncryptedContent": item["encrypted_content"]}});
            let summary: Vec<&str> = item["summary"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| s["text"].as_str())
                .collect();
            if summary.is_empty() {
                parts.push((String::new(), Some(options.clone())));
            }
            for text in summary {
                parts.push((text.to_owned(), Some(options.clone())));
            }
        }
        return parts;
    }
    match message["reasoning_content"].as_str() {
        Some(text) if !text.is_empty() => vec![(text.to_owned(), None)],
        _ => vec![],
    }
}

/// Node `toRecordInput`: the call's arguments as an object (`{}` otherwise).
pub fn tool_input(call: &Value) -> Value {
    let arguments = &call["function"]["arguments"];
    let parsed = match arguments {
        Value::String(raw) => serde_json::from_str(raw).unwrap_or(Value::Null),
        other => other.clone(),
    };
    if parsed.is_object() {
        parsed
    } else {
        json!({})
    }
}
