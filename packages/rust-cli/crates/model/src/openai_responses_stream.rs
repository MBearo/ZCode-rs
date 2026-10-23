//! OpenAI Responses 的 SSE 流（docs/specs/rust-cli/model.md §10.3）。
//!
//! 按 `data.type` 分发：`response.created` 产出 `MessageStart`；`output_item.added` / `.done` 圈定每个输出项，
//! 函数调用一项一个 tool_use 块；正文（`output_text` / `refusal`）与推理摘要（`reasoning_summary_text`、
//! `reasoning_text`）的增量进当前流；`response.completed` / `response.incomplete` 写出停止原因与用量后结束。
//! `error` 与 `response.failed` 按错误结束。不认识的事件与输出项（服务端工具等）忽略。

use std::collections::{HashMap, VecDeque};

use rust_cli_kernel_api::message::{StopReason, Usage};
use rust_cli_kernel_api::model::{ModelError, ModelEvent};
use rust_cli_net::sse::SseEvent;
use serde_json::{Value, json};

use crate::openai_blocks::{Blocks, Flow, number, protocol, take_str, usage};
use crate::openai_error;
use crate::protocol::StreamDecoder;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Message,
    Reasoning,
    Call,
    Skipped,
}

#[derive(Debug)]
struct Output {
    kind: Kind,
    /// 收到过增量；没有增量的项在 `.done` 时从完整内容补出。
    streamed: bool,
}

#[derive(Debug, Default)]
pub(crate) struct ResponsesStream {
    started: bool,
    finished: bool,
    blocks: Blocks,
    outputs: HashMap<u64, Output>,
}

fn output_index(event: &Value) -> Result<u64, ModelError> {
    event
        .get("output_index")
        .and_then(Value::as_u64)
        .ok_or_else(|| protocol("output event without an output_index"))
}

/// 完整输出项里的文本：消息的 `content[*].text` / `refusal`，推理的 `summary[*].text`。
fn item_text(item: &Value, field: &str) -> String {
    item.get(field)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|part| {
            part.get("text")
                .or_else(|| part.get("refusal"))
                .and_then(Value::as_str)
        })
        .collect()
}

impl ResponsesStream {
    fn start(&mut self, event: &mut Value, out: &mut VecDeque<ModelEvent>) {
        if self.started {
            return;
        }
        self.started = true;
        let mut response = event
            .get_mut("response")
            .map(Value::take)
            .unwrap_or_default();
        out.push_back(ModelEvent::MessageStart {
            id: take_str(&mut response, "id").unwrap_or_default(),
            model: take_str(&mut response, "model").unwrap_or_default(),
            usage: Usage::default(),
        });
    }

    fn added(
        &mut self,
        mut event: Value,
        out: &mut VecDeque<ModelEvent>,
    ) -> Result<(), ModelError> {
        let key = output_index(&event)?;
        if self.outputs.contains_key(&key) {
            return Err(protocol(format!("output item {key} added twice")));
        }
        let mut item = event.get_mut("item").map(Value::take).unwrap_or_default();
        let kind = match item.get("type").and_then(Value::as_str) {
            Some("message") => Kind::Message,
            Some("reasoning") => Kind::Reasoning,
            Some("function_call") => {
                let id = take_str(&mut item, "call_id").unwrap_or_default();
                let name = take_str(&mut item, "name").unwrap_or_default();
                self.blocks.tool_start(key, id, name, out)?;
                let arguments = take_str(&mut item, "arguments").unwrap_or_default();
                self.blocks.tool_arguments(key, arguments, out)?;
                Kind::Call
            }
            other => {
                tracing::warn!(item_type = ?other, "skipping an unsupported Responses output item");
                Kind::Skipped
            }
        };
        self.outputs.insert(
            key,
            Output {
                kind,
                streamed: false,
            },
        );
        Ok(())
    }

    fn delta(
        &mut self,
        mut event: Value,
        expected: Kind,
        out: &mut VecDeque<ModelEvent>,
    ) -> Result<(), ModelError> {
        let key = output_index(&event)?;
        let output = self
            .outputs
            .get_mut(&key)
            .ok_or_else(|| protocol(format!("delta for unknown output item {key}")))?;
        if output.kind == Kind::Skipped {
            return Ok(());
        }
        if output.kind != expected {
            return Err(protocol(format!("delta does not match output item {key}")));
        }
        output.streamed = true;
        let delta = take_str(&mut event, "delta").unwrap_or_default();
        match expected {
            Kind::Message => self.blocks.flow(Flow::Text, delta, out),
            Kind::Reasoning => self.blocks.flow(Flow::Thinking, delta, out),
            Kind::Call => self.blocks.tool_arguments(key, delta, out)?,
            Kind::Skipped => {}
        }
        Ok(())
    }

    fn done(&mut self, event: Value, out: &mut VecDeque<ModelEvent>) -> Result<(), ModelError> {
        let key = output_index(&event)?;
        let output = self
            .outputs
            .get(&key)
            .ok_or_else(|| protocol(format!("unknown output item {key} done")))?;
        let item = &event["item"];
        match output.kind {
            Kind::Call => self.blocks.tool_stop(key, out),
            Kind::Message | Kind::Reasoning => {
                let (flow, field) = if output.kind == Kind::Message {
                    (Flow::Text, "content")
                } else {
                    (Flow::Thinking, "summary")
                };
                if !output.streamed {
                    self.blocks.flow(flow, item_text(item, field), out);
                }
                self.blocks.end_flow(out);
            }
            Kind::Skipped => {}
        }
        Ok(())
    }

    fn completed(&mut self, event: &Value, out: &mut VecDeque<ModelEvent>) {
        let response = &event["response"];
        let reported = &response["usage"];
        let cached = number(&reported["input_tokens_details"], "cached_tokens");
        let usage = usage(
            number(reported, "input_tokens"),
            cached,
            number(reported, "output_tokens"),
        );
        let stop = match response["incomplete_details"]["reason"].as_str() {
            Some("max_output_tokens") => StopReason::MaxTokens,
            Some("content_filter") => StopReason::Refusal,
            Some(_) => StopReason::Unknown,
            None if self.blocks.has_tools() => StopReason::ToolUse,
            None => StopReason::EndTurn,
        };
        self.finished = true;
        self.blocks.finish(Some(stop), usage, out);
    }
}

impl StreamDecoder for ResponsesStream {
    fn decode(
        &mut self,
        event: SseEvent,
        out: &mut VecDeque<ModelEvent>,
    ) -> Result<(), ModelError> {
        if self.finished {
            return Ok(());
        }
        let mut value: Value = serde_json::from_str(&event.data)
            .map_err(|error| protocol(format!("invalid event JSON: {error}")))?;
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        match kind.as_str() {
            "error" => return Err(openai_error::from_stream(&value)),
            "response.failed" => {
                let error = value["response"]["error"].take();
                return Err(openai_error::from_stream(&json!({ "error": error })));
            }
            "response.created" | "response.in_progress" => self.start(&mut value, out),
            _ if !self.started => {
                return Err(protocol(format!("{kind} before response.created")));
            }
            "response.output_item.added" => self.added(value, out)?,
            "response.output_text.delta" | "response.refusal.delta" => {
                self.delta(value, Kind::Message, out)?;
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                self.delta(value, Kind::Reasoning, out)?;
            }
            "response.function_call_arguments.delta" => self.delta(value, Kind::Call, out)?,
            "response.output_item.done" => self.done(value, out)?,
            "response.completed" | "response.incomplete" => self.completed(&value, out),
            _ => {}
        }
        Ok(())
    }

    fn finished(&self) -> bool {
        self.finished
    }
}

#[cfg(test)]
#[path = "openai_responses_stream_tests.rs"]
mod tests;
