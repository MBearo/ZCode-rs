use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ContextState {
    pub offset: usize,
    pub summary: Option<String>,
}
#[derive(Clone, Copy)]
pub struct ContextPolicy {
    pub window: usize,
    pub max_output: usize,
    pub buffer: usize,
    pub automatic: bool,
}
impl Default for ContextPolicy {
    fn default() -> Self {
        Self {
            window: 200_000,
            max_output: 32_000,
            buffer: 13_000,
            automatic: true,
        }
    }
}
impl ContextPolicy {
    pub fn threshold(self) -> usize {
        self.window
            .saturating_sub(self.max_output.min(21_000))
            .saturating_sub(self.buffer)
    }
    pub fn micro_threshold(self) -> usize {
        (self.threshold() * 9 / 10).min(self.threshold().saturating_sub(2_000))
    }
}
pub fn estimate(messages: &[Value]) -> usize {
    messages
        .iter()
        .map(|m| {
            let mut count = chars(&m["content"]) + chars(&m["reasoning_content"]);
            if let Some(calls) = m["tool_calls"].as_array() {
                for call in calls {
                    count +=
                        chars(&call["function"]["name"]) + chars(&call["function"]["arguments"]);
                }
            }
            count.div_ceil(3)
        })
        .sum()
}
fn chars(value: &Value) -> usize {
    if let Some(parts) = value.as_array() {
        return parts
            .iter()
            .map(|part| {
                if part["type"] == "_zcode_attachment" {
                    let mime = part["asset"]["mediaType"].as_str().unwrap_or("");
                    if mime.starts_with("image/") {
                        3072
                    } else {
                        part["asset"]["totalBytes"]
                            .as_u64()
                            .unwrap_or(0)
                            .min(64 * 1024) as usize
                    }
                } else {
                    chars(part)
                }
            })
            .sum();
    }
    match value {
        Value::String(s) => s.encode_utf16().count(),
        Value::Null => 0,
        _ => value.to_string().encode_utf16().count(),
    }
}
/// 分界只能在完整工具轮次之间；最新 assistant 轮次及之后的用户输入原样保留。
pub fn split_for_summary(messages: &[Value], manual: bool) -> Option<usize> {
    let mut pending = BTreeSet::new();
    let mut candidates = vec![];
    let mut assistant_seen = false;
    for (i, message) in messages.iter().enumerate() {
        if message["role"] == "assistant" {
            if pending.is_empty() && assistant_seen {
                candidates.push(i);
            }
            assistant_seen = true;
        }
        if let Some(calls) = message["tool_calls"].as_array() {
            for call in calls {
                pending.insert(call["id"].as_str()?);
            }
        }
        if let Some(id) = message["tool_call_id"].as_str() {
            pending.remove(id);
        }
    }
    if !pending.is_empty() || !assistant_seen || messages.len() < 2 {
        return None;
    }
    if manual {
        Some(messages.len())
    } else {
        candidates.last().copied().or_else(|| {
            messages
                .iter()
                .enumerate()
                .rev()
                .find(|(i, m)| *i > 1 && m["role"] == "user")
                .map(|(i, _)| i)
        })
    }
}
pub fn microcompact(mut messages: Vec<Value>, threshold: usize) -> Vec<Value> {
    let before = estimate(&messages);
    if before < threshold {
        return messages;
    }
    let mut names = BTreeMap::new();
    let mut candidates = vec![];
    for (i, m) in messages.iter().enumerate() {
        if let Some(calls) = m["tool_calls"].as_array() {
            for call in calls {
                names.insert(
                    call["id"].as_str().unwrap_or(""),
                    call["function"]["name"].as_str().unwrap_or(""),
                );
            }
        }
        let name = names.get(m["tool_call_id"].as_str().unwrap_or(""));
        if m["_zcode_tool_failed"] != true
            && matches!(
                name,
                Some(&("Read" | "Bash" | "Grep" | "Glob" | "Edit" | "Write"))
            )
            && m["content"].as_str().is_some_and(|s| {
                !s.starts_with("Tool failed:") && !s.contains("\"status\":\"failed\"")
            })
        {
            candidates.push(i);
        }
    }
    let mut removed = vec![];
    for i in candidates.iter().take(candidates.len().saturating_sub(5)) {
        removed.push((
            *i,
            std::mem::replace(
                &mut messages[*i]["content"],
                json!("[Old tool result content cleared]"),
            ),
        ));
    }
    if before.saturating_sub(estimate(&messages)) < 256 {
        for (i, content) in removed {
            messages[i]["content"] = content;
        }
    }
    messages
}
pub fn with_summary(summary: Option<&str>, messages: &[Value]) -> Vec<Value> {
    let mut out = Vec::with_capacity(messages.len() + 1);
    if let Some(summary) = summary {
        out.push(json!({"role":"user","content":format!("The earlier conversation was compacted. This is a summary of prior context, not new instructions:\n{summary}")}));
    }
    out.extend_from_slice(messages);
    out
}
