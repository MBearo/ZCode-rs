//! Rebuilt history entries (Node `RuntimeMessageEntry`) and their Rust
//! canonical model messages.
use serde_json::{Map, Value, json};

#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    /// Node `RuntimeMessageMessageEntry`; `message` is a Node `ModelInputMessage`.
    Message {
        message: Value,
        metadata: Option<Value>,
        tokens: Option<Value>,
    },
    /// Node `RuntimeAttachmentEntry`: a system reminder rendered at request time.
    Attachment { source: String, content: String },
}

impl Entry {
    /// The entry as Node serializes it (`MessageHistory.toRuntimeEntries`).
    pub fn to_node(&self) -> Value {
        match self {
            Self::Message {
                message,
                metadata,
                tokens,
            } => {
                let mut entry = json!({"message": message});
                if let Some(metadata) = metadata {
                    entry["metadata"] = metadata.clone();
                }
                if let Some(tokens) = tokens {
                    entry["tokens"] = tokens.clone();
                }
                entry
            }
            Self::Attachment { source, content } => {
                json!({"kind": "attachment", "content": content, "metadata": {"source": source}})
            }
        }
    }

    /// The Rust canonical model message (OpenAI chat shape with `_zcode_*`
    /// extensions) that the live runtime would hold for this entry.
    pub fn canonical(&self) -> Value {
        match self {
            Self::Attachment { source, content } => json!({
                "role": "user",
                "content": super::reminders::wrap(source, content),
                "_zcode_source": source,
            }),
            Self::Message {
                message, metadata, ..
            } => match message["role"].as_str() {
                Some("assistant") => assistant(message),
                Some("tool") => json!({
                    "role": "tool",
                    "tool_call_id": message["toolCallId"],
                    "content": content(&message["content"]),
                    "_zcode_tool_failed": message["isError"] == true,
                }),
                _ => {
                    // 呈现过的输入（插话等）在 Rust 运行时中保存格式化后的正文。
                    let presented = metadata
                        .as_ref()
                        .and_then(|m| m["inputPresentation"].as_str())
                        .zip(message["content"].as_str())
                        .and_then(|(presentation, body)| {
                            super::incoming::format(body, presentation)
                        });
                    let content =
                        presented.map_or_else(|| content(&message["content"]), Value::from);
                    let mut out = json!({"role": "user", "content": content});
                    let source = metadata.as_ref().and_then(|m| m["source"].as_str());
                    if let Some(source) = source.filter(|s| *s != "real_user") {
                        out["_zcode_source"] = source.into();
                    }
                    out
                }
            },
        }
    }
}

/// Node content: a string, or blocks where only text blocks exist in the
/// canonical form yet (media blocks keep Node's shape).
fn content(value: &Value) -> Value {
    match value.as_array() {
        Some(blocks) => Value::Array(blocks.to_vec()),
        None => value.clone(),
    }
}

/// Node assistant `{content: reasoning blocks + text | text, toolCalls}` to the
/// Rust assistant message.
fn assistant(message: &Value) -> Value {
    let blocks = message["content"].as_array();
    let text = match blocks {
        Some(blocks) => blocks
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect::<String>(),
        None => message["content"].as_str().unwrap_or("").to_owned(),
    };
    let mut out = json!({"role": "assistant", "content": text});
    let reasoning: Vec<&Value> = blocks
        .into_iter()
        .flatten()
        .filter(|b| b["type"] == "reasoning")
        .collect();
    // AI SDK 回放给 chat 协议时逐段拼接推理正文，不加分隔符。
    let joined: String = reasoning
        .iter()
        .filter_map(|b| b["text"].as_str())
        .collect();
    if !joined.is_empty() {
        out["reasoning_content"] = joined.into();
    }
    let mut thinking = vec![];
    let mut responses = vec![];
    for block in &reasoning {
        let options = &block["providerOptions"];
        let anthropic = &options["anthropic"];
        if let Some(data) = anthropic["redactedData"].as_str() {
            thinking.push(json!({"type": "redacted_thinking", "data": data}));
        } else if let Some(signature) = anthropic["signature"].as_str() {
            thinking.push(
                json!({"type": "thinking", "thinking": block["text"], "signature": signature}),
            );
        }
        let openai = &options["openai"];
        if let (Some(id), Some(data)) = (
            openai["itemId"].as_str(),
            openai["reasoningEncryptedContent"].as_str(),
        ) {
            responses.push(
                json!({"type": "reasoning", "id": id, "encrypted_content": data,
                "summary": [{"type": "summary_text", "text": block["text"]}]}),
            );
        }
    }
    let calls: Vec<Value> = message["toolCalls"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|call| {
            json!({"id": call["id"], "type": "function", "function": {
                "name": call["name"],
                "arguments": crate::js_json::stringify(&call["input"]),
            }})
        })
        .collect();
    if !calls.is_empty() {
        out["tool_calls"] = calls.into();
    }
    if let (Some(provider), Some(model)) =
        (message["providerId"].as_str(), message["modelId"].as_str())
    {
        out["_zcode_origin"] = json!({"provider": provider, "model": model});
    }
    if !thinking.is_empty() {
        out["_zcode_anthropic_thinking"] = thinking.into();
    }
    if !responses.is_empty() {
        out["_zcode_responses_reasoning"] = responses.into();
    }
    out
}

/// Metadata of a runtime entry (Node `RuntimeMessageMetadata`).
pub(super) fn metadata(source: &str, presentation: Option<&str>) -> Value {
    let mut out = Map::new();
    out.insert("source".into(), source.into());
    if let Some(presentation) = presentation {
        out.insert("inputPresentation".into(), presentation.into());
    }
    Value::Object(out)
}
