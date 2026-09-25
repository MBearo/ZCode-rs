//! The main conversation's protocol usage state (spec rust-m9-usage-logs §4):
//! Node `getModelUsageContextTokens`, the main-turn cache hit aggregate
//! (`recordMainTurnCacheHitUsage`, `mainTurnCacheHitAggregateFromMessages`)
//! and the context breakdown (`buildContextUsageBreakdownFromSnapshot`).
use super::tool_meta;
use crate::js_json::stringify;
use serde_json::{Map, Value, json};

fn floor(value: &Value) -> Option<f64> {
    value.as_f64().filter(|n| n.is_finite()).map(f64::floor)
}

fn positive(value: &Value) -> Option<u64> {
    floor(value).filter(|n| *n > 0.0).map(|n| n as u64)
}

fn non_negative(value: &Value) -> Option<u64> {
    floor(value).filter(|n| *n >= 0.0).map(|n| n as u64)
}

/// The length of `text` as a JavaScript string (UTF-16 code units).
pub fn js_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Node `getModelUsageInputWindowTokens` over a normalized `ModelUsage`.
pub fn input_window(usage: &Value) -> Option<u64> {
    if let Some(input) = positive(&usage["inputTokens"]) {
        return Some(input);
    }
    if let Some(total) = positive(&usage["totalTokens"]) {
        let output = non_negative(&usage["outputTokens"]).unwrap_or(0);
        return Some(total.saturating_sub(output));
    }
    let cache = non_negative(&usage["cacheReadTokens"]).unwrap_or(0)
        + non_negative(&usage["cacheWriteTokens"]).unwrap_or(0);
    (cache > 0).then_some(cache)
}

/// Node `getModelUsageContextTokens`: the context a request occupied.
pub fn context_tokens(usage: &Value) -> Option<u64> {
    if !usage.is_object() {
        return None;
    }
    let tokens =
        input_window(usage).unwrap_or(0) + non_negative(&usage["outputTokens"]).unwrap_or(0);
    if tokens > 0 {
        return Some(tokens);
    }
    positive(&usage["totalTokens"])
}

/// The cache use of one main-turn request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheUse {
    pub input: u64,
    pub read: u64,
    pub write: u64,
}

impl CacheUse {
    fn nonzero(self) -> Option<Self> {
        (self.input > 0 || self.read > 0 || self.write > 0).then_some(self)
    }

    /// Node `recordMainTurnCacheHitUsage` over a normalized `ModelUsage`.
    pub fn of(usage: &Value) -> Option<Self> {
        Self {
            input: input_window(usage).unwrap_or(0),
            read: non_negative(&usage["cacheReadTokens"]).unwrap_or(0),
            write: non_negative(&usage["cacheWriteTokens"]).unwrap_or(0),
        }
        .nonzero()
    }

    /// Node `mainTurnCacheHitAggregateFromMessages` over one stored assistant
    /// message's `tokens`.
    pub fn stored(tokens: &Value) -> Option<Self> {
        Self {
            input: non_negative(&tokens["input"]).unwrap_or(0),
            read: non_negative(&tokens["cache"]["read"]).unwrap_or(0),
            write: non_negative(&tokens["cache"]["write"]).unwrap_or(0),
        }
        .nonzero()
    }
}

/// The main-turn requests' cache use, each at the model message index of its
/// assistant message (Node `mainTurnCacheHitAggregate`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CacheHits {
    entries: Vec<(usize, CacheUse)>,
}

fn ratio(numerator: u64, denominator: u64) -> Value {
    if denominator > 0 {
        json!(numerator as f64 / denominator as f64)
    } else {
        Value::Null
    }
}

impl CacheHits {
    pub fn new(entries: Vec<(usize, CacheUse)>) -> Self {
        Self { entries }
    }

    /// Records the request whose assistant message is at `message`; the
    /// `cacheHit` member of its `ModelComplete` (`None` without cache use).
    pub fn record(&mut self, message: usize, usage: &Value) -> Option<Value> {
        let latest = CacheUse::of(usage)?;
        self.entries.push((message, latest));
        let total = self
            .entries
            .iter()
            .fold(CacheUse::default(), |sum, (_, u)| CacheUse {
                input: sum.input + u.input,
                read: sum.read + u.read,
                write: sum.write + u.write,
            });
        Some(
            json!({"inputTokens": latest.input, "cacheReadTokens": latest.read,
            "cacheWriteTokens": latest.write, "latestHitRate": ratio(latest.read, latest.input),
            "hitRate": ratio(total.read, total.input), "hitRateRequestCount": self.entries.len(),
            "totalInputTokens": total.input, "totalCacheReadTokens": total.read,
            "totalCacheWriteTokens": total.write}),
        )
    }

    /// Drops the requests whose messages were removed (`messages.truncate(len)`):
    /// Node rebuilds the aggregate from the remaining branch after a rewind.
    pub fn truncate(&mut self, len: usize) {
        self.entries.retain(|(message, _)| *message < len);
    }
}

/// Characters of one request's prompt sections (JavaScript string lengths).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SectionChars {
    pub system: usize,
    pub meta_user: usize,
    pub skills: usize,
}

/// The first line of a presented task notification (Node counts it with
/// the messages although it is wrapped as a reminder).
const TASK_NOTIFICATION: &str = "[SYSTEM NOTIFICATION - NOT USER INPUT]";

fn text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

/// Node `ModelMessageContent` of a model message's content.
fn node_content(content: &Value) -> Value {
    let Some(parts) = content.as_array() else {
        return content.as_str().unwrap_or("").into();
    };
    parts
        .iter()
        .map(|part| match part["type"].as_str() {
            Some("image_url") => {
                json!({"type": "image", "source": {"type": "url", "url": part["image_url"]["url"]}})
            }
            _ => json!({"type": "text", "text": part["text"].as_str().unwrap_or("")}),
        })
        .collect()
}

/// The characters Node's messages category counts (`buildMessageRoleBreakdown`
/// of `{role, content, toolCalls, toolCallId, toolName}`).
fn message_chars(messages: &[Value]) -> usize {
    let mut names = Map::new();
    let mut chars = 0;
    for message in messages {
        let role = message["role"].as_str().unwrap_or("");
        if role == "system" {
            continue;
        }
        if role == "user" {
            let text = text(&message["content"]);
            let reminder = text.trim_start().starts_with("<system-reminder>");
            if reminder && !text.contains(TASK_NOTIFICATION) {
                continue;
            }
        }
        let mut node = json!({"role": role, "content": node_content(&message["content"])});
        if let Some(calls) = message["tool_calls"].as_array().filter(|c| !c.is_empty()) {
            let calls: Vec<Value> = calls
                .iter()
                .map(|call| {
                    let name = call["function"]["name"].clone();
                    names.insert(call["id"].as_str().unwrap_or("").into(), name.clone());
                    let arguments = call["function"]["arguments"].as_str().unwrap_or("{}");
                    let input: Value = serde_json::from_str(arguments).unwrap_or(json!({}));
                    json!({"id": call["id"], "name": name, "input": input})
                })
                .collect();
            node["toolCalls"] = calls.into();
        }
        if let Some(id) = message["tool_call_id"].as_str() {
            node["toolCallId"] = id.into();
            if let Some(name) = names.get(id) {
                node["toolName"] = name.clone();
            }
        }
        chars += js_len(&stringify(&node));
    }
    chars
}

/// The characters of one tool's contract (`buildToolUsageDetail`), and
/// whether it is an MCP tool.
fn tool_chars(definition: &Value) -> (usize, bool) {
    let function = &definition["function"];
    let name = function["name"].as_str().unwrap_or("");
    let mut contract = json!({"name": name, "description": function["description"],
        "inputSchema": function["parameters"]});
    if let Some(meta) = tool_meta(name, None) {
        contract["readOnly"] = meta.read_only.into();
        contract["destructive"] = meta.destructive.into();
        contract["sideEffectScope"] = meta.scope.into();
    }
    (js_len(&stringify(&contract)), name.starts_with("mcp__"))
}

/// Node `contextUsageBreakdown` of a main-turn request: the categories with
/// characters, in Node's order.
pub fn breakdown(sections: SectionChars, tools: &[Value], messages: &[Value]) -> Vec<Value> {
    let (mut system_tools, mut mcp_tools) = (0, 0);
    for definition in tools {
        match tool_chars(definition) {
            (chars, true) => mcp_tools += chars,
            (chars, false) => system_tools += chars,
        }
    }
    [
        ("system_prompt", sections.system),
        ("meta_user_context", sections.meta_user),
        ("skills", sections.skills),
        ("system_tool_schemas", system_tools),
        ("mcp_tool_schemas", mcp_tools),
        ("messages", message_chars(messages)),
    ]
    .into_iter()
    .filter(|(_, chars)| *chars > 0)
    .map(|(source, chars)| json!({"source": source, "chars": chars}))
    .collect()
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod tests;
