use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

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
                } else if let Some(data) = part["dataUrl"].as_str() {
                    // 冷读取的 Node 媒体块与 _zcode_attachment 同样估算，不按 base64 长度计。
                    if part["type"] == "image" {
                        3072
                    } else {
                        (data.len() / 4 * 3).min(64 * 1024)
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
pub fn with_summary(summary: Option<&str>, messages: &[Value]) -> Vec<Value> {
    let mut out = Vec::with_capacity(messages.len() + 1);
    // Node 的摘要消息（新压缩与 TS 导入）原文发送；旧 Rust 摘要只有正文，保留原前缀。
    match summary {
        Some(summary) if summary.starts_with(super::compact::SUMMARY_HEADER) => {
            out.push(json!({"role":"user","content":summary}));
        }
        Some(summary) => out.push(json!({"role":"user","content":format!("The earlier conversation was compacted. This is a summary of prior context, not new instructions:\n{summary}")})),
        None => {}
    }
    out.extend_from_slice(messages);
    out
}
