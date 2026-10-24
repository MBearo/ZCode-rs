//! Anthropic SSE 流解码（docs/specs/rust-cli/model.md §3）。
//!
//! 按 SSE 的 `event:` 名分发（与 SDK 相同，未知或缺少名字的事件忽略），
//! `content_block_start` 里的初始文本、thinking 与 tool 输入被忽略（SDK 有时在 start 与 delta 里各给
//! 一次同样的文本），thinking 块的 signature 初始化为空串。`message_delta` 的用量合并后
//! 输出，所以 [`ModelEvent::MessageDelta`] 的 `usage` 是这次响应的累计用量。

use std::collections::{HashMap, HashSet, VecDeque};

use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::message::{ContentBlock, StopReason, ToolResultContent, Usage};
use rust_cli_kernel_api::model::{BlockDelta, ModelError, ModelEvent};
use rust_cli_net::sse::SseEvent;
use serde_json::{Map, Value};

use crate::anthropic_error;
use crate::protocol::StreamDecoder;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Thinking,
    RedactedThinking,
    ToolUse,
    /// 服务端工具的调用与结果（WebSearch 的旁路请求，tools-more.md §5；端点在主回复里自己产生的，§4.1）：开始事件带
    /// 完整内容，增量（服务端拼查询的 input_json）不输出。
    ServerTool,
    /// 内核没有对应形状的块：它的增量与结束事件都不输出。
    Skipped,
}

#[derive(Debug, Default)]
pub(crate) struct AnthropicStream {
    started: bool,
    finished: bool,
    usage: Usage,
    blocks: HashMap<usize, Kind>,
    /// 这次响应里已开始的 `server_tool_use` 的 ID：认出端点非标准的 `tool_result` 块（工作线 F90，model.md §4.1）。
    server_calls: HashSet<String>,
}

fn protocol(message: impl Into<String>) -> ModelError {
    ModelError::Protocol {
        message: message.into(),
    }
}

/// 取出字符串字段（移出，不复制）。
fn take_str(object: &mut Value, name: &str) -> Option<String> {
    match object.get_mut(name).map(Value::take) {
        Some(Value::String(text)) => Some(text),
        _ => None,
    }
}

fn index(event: &Value) -> Result<usize, ModelError> {
    event
        .get("index")
        .and_then(Value::as_u64)
        .and_then(|index| usize::try_from(index).ok())
        .ok_or_else(|| protocol("content block event without an index"))
}

/// 服务端结果块的 `content`，按 tool_result 的标准内容解（model.md §4.1）：字符串是一个文本块；数组的每项按
/// `text`、`image`、`document`、`tool_reference` 解，解不了的项写成它的 JSON 文本；没有内容时为空。
fn server_result_content(content: Value) -> Vec<ToolResultContent> {
    match content {
        Value::Null => Vec::new(),
        Value::String(text) => vec![ToolResultContent::Text { text }],
        Value::Array(items) => items
            .into_iter()
            .map(|item| {
                serde_json::from_value(item.clone()).unwrap_or_else(|_| ToolResultContent::Text {
                    text: item.to_string(),
                })
            })
            .collect(),
        other => vec![ToolResultContent::Text {
            text: other.to_string(),
        }],
    }
}

/// 用量字段：数字时取值，`null` 或缺失时为空。
fn count(usage: &Value, name: &str) -> Option<u64> {
    usage.get(name).and_then(Value::as_u64)
}

impl AnthropicStream {
    fn message_start(&mut self, mut event: Value, out: &mut VecDeque<ModelEvent>) {
        let mut message = event
            .get_mut("message")
            .map(Value::take)
            .unwrap_or_default();
        let usage = message.get("usage").cloned().unwrap_or_default();
        self.usage = Usage {
            input_tokens: count(&usage, "input_tokens").unwrap_or(0),
            output_tokens: count(&usage, "output_tokens").unwrap_or(0),
            cache_creation_input_tokens: count(&usage, "cache_creation_input_tokens").unwrap_or(0),
            cache_read_input_tokens: count(&usage, "cache_read_input_tokens").unwrap_or(0),
        };
        self.started = true;
        out.push_back(ModelEvent::MessageStart {
            id: take_str(&mut message, "id").unwrap_or_default(),
            model: take_str(&mut message, "model").unwrap_or_default(),
            usage: self.usage,
        });
    }

    fn block_start(
        &mut self,
        mut event: Value,
        out: &mut VecDeque<ModelEvent>,
    ) -> Result<(), ModelError> {
        let index = index(&event)?;
        if self.blocks.contains_key(&index) {
            return Err(protocol(format!("content block {index} started twice")));
        }
        let mut block = event
            .get_mut("content_block")
            .map(Value::take)
            .unwrap_or_default();
        let block_type = take_str(&mut block, "type").unwrap_or_default();
        let (kind, start) = match block_type.as_str() {
            "text" => (
                Kind::Text,
                Some(ContentBlock::Text {
                    text: String::new(),
                }),
            ),
            "thinking" => (
                Kind::Thinking,
                Some(ContentBlock::Thinking {
                    thinking: String::new(),
                    signature: Some(String::new()),
                }),
            ),
            "redacted_thinking" => {
                let data = take_str(&mut block, "data")
                    .ok_or_else(|| protocol("redacted_thinking block without data"))?;
                (
                    Kind::RedactedThinking,
                    Some(ContentBlock::RedactedThinking { data }),
                )
            }
            "tool_use" => {
                let id = take_str(&mut block, "id")
                    .ok_or_else(|| protocol("tool_use block without an id"))?;
                let name = take_str(&mut block, "name")
                    .ok_or_else(|| protocol("tool_use block without a name"))?;
                // 输入由 input_json_delta 拼接；拼接结果为空时等于 `{}`（model.md §3）。
                let start = ContentBlock::ToolUse {
                    id: ToolUseId::new(id),
                    name,
                    input: Value::Object(Map::new()),
                };
                (Kind::ToolUse, Some(start))
            }
            "server_tool_use" => {
                let id = take_str(&mut block, "id").unwrap_or_default();
                let name = take_str(&mut block, "name").unwrap_or_default();
                let input = block.get_mut("input").map(Value::take).unwrap_or_default();
                self.server_calls.insert(id.clone());
                (
                    Kind::ServerTool,
                    Some(ContentBlock::ServerToolUse { id, name, input }),
                )
            }
            // 修 bug（工作线 F90，model.md §4.1）：GLM 在 assistant 一侧用 `tool_result` 块返回服务端调用的结果。以前
            // 当成未知块跳过，跳过的序号在组装器里留下空槽，流结束时报 `stream ended with an unfinished block`。
            // `tool_use_id` 引用这次响应里的 `server_tool_use` 时，按那次调用的结果解出；对不上的照旧跳过。
            "tool_result"
                if block
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| self.server_calls.contains(id)) =>
            {
                let tool_use_id = take_str(&mut block, "tool_use_id").unwrap_or_default();
                let is_error = block
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let content = server_result_content(
                    block
                        .get_mut("content")
                        .map(Value::take)
                        .unwrap_or_default(),
                );
                (
                    Kind::ServerTool,
                    Some(ContentBlock::ToolResult {
                        tool_use_id: ToolUseId::new(tool_use_id),
                        content,
                        is_error,
                    }),
                )
            }
            "web_search_tool_result" => {
                let tool_use_id = take_str(&mut block, "tool_use_id").unwrap_or_default();
                let content = block
                    .get_mut("content")
                    .map(Value::take)
                    .unwrap_or_default();
                (
                    Kind::ServerTool,
                    Some(ContentBlock::WebSearchToolResult {
                        tool_use_id,
                        content,
                    }),
                )
            }
            other => {
                tracing::warn!(
                    block_type = other,
                    index,
                    "skipping an unsupported content block"
                );
                (Kind::Skipped, None)
            }
        };
        self.blocks.insert(index, kind);
        if let Some(block) = start {
            out.push_back(ModelEvent::BlockStart { index, block });
        }
        Ok(())
    }

    fn block_delta(
        &mut self,
        mut event: Value,
        out: &mut VecDeque<ModelEvent>,
    ) -> Result<(), ModelError> {
        let index = index(&event)?;
        let kind = *self
            .blocks
            .get(&index)
            .ok_or_else(|| protocol(format!("delta for unknown content block {index}")))?;
        let mut delta = event.get_mut("delta").map(Value::take).unwrap_or_default();
        let delta_type = take_str(&mut delta, "type").unwrap_or_default();
        let (expected, field, wrap): (Kind, &str, fn(String) -> BlockDelta) =
            match delta_type.as_str() {
                "text_delta" => (Kind::Text, "text", BlockDelta::Text),
                "thinking_delta" => (Kind::Thinking, "thinking", BlockDelta::Thinking),
                "signature_delta" => (Kind::Thinking, "signature", BlockDelta::Signature),
                "input_json_delta" => (Kind::ToolUse, "partial_json", BlockDelta::InputJson),
                // citations_delta 等：不处理。
                _ => return Ok(()),
            };
        if matches!(kind, Kind::Skipped | Kind::ServerTool) {
            return Ok(());
        }
        if kind != expected {
            return Err(protocol(format!(
                "{delta_type} for a {kind:?} block at {index}"
            )));
        }
        let value = take_str(&mut delta, field)
            .ok_or_else(|| protocol(format!("{delta_type} without {field}")))?;
        out.push_back(ModelEvent::BlockDelta {
            index,
            delta: wrap(value),
        });
        Ok(())
    }

    fn block_stop(
        &mut self,
        event: &Value,
        out: &mut VecDeque<ModelEvent>,
    ) -> Result<(), ModelError> {
        let index = index(event)?;
        match self.blocks.get(&index) {
            None => Err(protocol(format!("stop for unknown content block {index}"))),
            Some(Kind::Skipped) => Ok(()),
            Some(_) => {
                out.push_back(ModelEvent::BlockStop { index });
                Ok(())
            }
        }
    }

    /// 用量合并：输入与缓存 token 只在大于 0 时覆盖，输出 token 只要给出就覆盖。
    fn message_delta(&mut self, mut event: Value, out: &mut VecDeque<ModelEvent>) {
        let usage = event.get("usage").cloned().unwrap_or_default();
        let positive = |name: &str, current: u64| {
            count(&usage, name)
                .filter(|value| *value > 0)
                .unwrap_or(current)
        };
        self.usage = Usage {
            input_tokens: positive("input_tokens", self.usage.input_tokens),
            cache_creation_input_tokens: positive(
                "cache_creation_input_tokens",
                self.usage.cache_creation_input_tokens,
            ),
            cache_read_input_tokens: positive(
                "cache_read_input_tokens",
                self.usage.cache_read_input_tokens,
            ),
            output_tokens: count(&usage, "output_tokens").unwrap_or(self.usage.output_tokens),
        };
        let stop_reason = event
            .get_mut("delta")
            .and_then(|delta| delta.get_mut("stop_reason"))
            .map(Value::take)
            .filter(|value| !value.is_null())
            .map(|value| serde_json::from_value(value).unwrap_or(StopReason::Unknown));
        out.push_back(ModelEvent::MessageDelta {
            stop_reason,
            usage: self.usage,
        });
    }
}

/// SDK 当作消息事件解析的 SSE 事件名。
const MESSAGE_EVENTS: [&str; 7] = [
    "message_start",
    "message_delta",
    "message_stop",
    "content_block_start",
    "content_block_delta",
    "content_block_stop",
    "message",
];

impl StreamDecoder for AnthropicStream {
    fn decode(
        &mut self,
        event: SseEvent,
        out: &mut VecDeque<ModelEvent>,
    ) -> Result<(), ModelError> {
        let name = event.event.as_deref().unwrap_or_default();
        if name == "error" {
            return Err(anthropic_error::from_stream(&event.data));
        }
        if self.finished || !MESSAGE_EVENTS.contains(&name) {
            // ping 与未知事件：SDK 跳过。
            return Ok(());
        }
        let value: Value = serde_json::from_str(&event.data)
            .map_err(|error| protocol(format!("invalid {name} event: {error}")))?;
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if kind != "message_start" && !self.started && MESSAGE_EVENTS[..6].contains(&kind.as_str())
        {
            return Err(protocol(format!("{kind} before message_start")));
        }
        match kind.as_str() {
            "message_start" => self.message_start(value, out),
            "content_block_start" => self.block_start(value, out)?,
            "content_block_delta" => self.block_delta(value, out)?,
            "content_block_stop" => self.block_stop(&value, out)?,
            "message_delta" => self.message_delta(value, out),
            "message_stop" => {
                self.finished = true;
                out.push_back(ModelEvent::MessageStop);
            }
            _ => {}
        }
        Ok(())
    }

    fn finished(&self) -> bool {
        self.finished
    }
}

#[cfg(test)]
#[path = "anthropic_stream_tests.rs"]
mod tests;
