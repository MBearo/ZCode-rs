//! OpenAI Chat Completions 的 SSE 流（docs/specs/rust-cli/model.md §10.3）。
//!
//! 每个 `data:` 是一个 chunk，流以 `data: [DONE]` 结束（与 Node / OpenAI SDK 相同，没有 `[DONE]` 的流
//! 按中途断开处理）。第一个 chunk 产出 `MessageStart`；`delta.reasoning_content`（或 `delta.reasoning`）
//! 是推理，`delta.content` 是正文，`delta.tool_calls[*]` 按 `index` 对应函数调用；`finish_reason` 记下，
//! `[DONE]` 时与最后的 `usage`（`stream_options.include_usage`）一起写出。chunk 里带 `error` 时按错误结束。

use std::collections::VecDeque;

use rust_cli_kernel_api::message::{StopReason, Usage};
use rust_cli_kernel_api::model::ModelEvent;
use rust_cli_kernel_api::provider::WireProtocol;
use rust_cli_net::sse::SseEvent;
use serde_json::Value;

use crate::errors::{self, Finish, Vendor, Verdict};
use crate::openai_blocks::{Blocks, Flow, number, protocol, take_str, usage};
use crate::protocol::StreamDecoder;

#[derive(Debug, Default)]
pub(crate) struct ChatStream {
    /// 错误与结束原因经这个厂商的分类器（provider-errors.md §2.2，修复线 F113）。
    vendor: Vendor,
    started: bool,
    finished: bool,
    blocks: Blocks,
    stop_reason: Option<StopReason>,
    usage: Usage,
}

impl ChatStream {
    pub(crate) fn new(vendor: Vendor) -> Self {
        Self {
            vendor,
            ..Self::default()
        }
    }

    fn chunk(&mut self, mut chunk: Value, out: &mut VecDeque<ModelEvent>) -> Result<(), Verdict> {
        if chunk.get("error").is_some_and(|error| !error.is_null()) {
            let raw = chunk.to_string();
            return Err(errors::classify_stream_error(
                self.vendor,
                WireProtocol::OpenAiChatCompletions,
                &chunk,
                &raw,
            ));
        }
        if !self.started {
            self.started = true;
            out.push_back(ModelEvent::MessageStart {
                id: take_str(&mut chunk, "id").unwrap_or_default(),
                model: take_str(&mut chunk, "model").unwrap_or_default(),
                usage: Usage::default(),
            });
        }
        if let Some(reported) = chunk.get("usage").filter(|value| value.is_object()) {
            let cached = reported
                .get("prompt_tokens_details")
                .map_or(0, |details| number(details, "cached_tokens"));
            self.usage = usage(
                number(reported, "prompt_tokens"),
                cached,
                number(reported, "completion_tokens"),
            );
        }
        let Some(choice) = chunk
            .get_mut("choices")
            .and_then(Value::as_array_mut)
            .and_then(|choices| choices.first_mut())
        else {
            return Ok(());
        };
        // `finish_reason` 经厂商的分类器换算（修复线 F113：以前是这里自己的表；智谱的 `sensitive` 与 `network_error`
        // 都是 `Unknown`，前者静默结束，后者把截断的内容当正常完成）。以结束原因报的失败按流里的错误返回。
        let mut failed = None;
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            match errors::classify_finish(self.vendor, reason) {
                Finish::Stop(reason) => self.stop_reason = Some(reason),
                Finish::Failed(verdict) => failed = Some(verdict),
            }
        }
        let mut delta = choice.get_mut("delta").map(Value::take).unwrap_or_default();
        let reasoning = take_str(&mut delta, "reasoning_content")
            .or_else(|| take_str(&mut delta, "reasoning"))
            .unwrap_or_default();
        self.blocks.flow(Flow::Thinking, reasoning, out);
        let text = take_str(&mut delta, "content").unwrap_or_default();
        self.blocks.flow(Flow::Text, text, out);
        if let Some(Value::Array(calls)) = delta.get_mut("tool_calls").map(Value::take) {
            for call in calls {
                self.tool_call(call, out)?;
            }
        }
        // 同一个 chunk 里的增量先交出，再报以结束原因报的失败。
        failed.map_or(Ok(()), Err)
    }

    fn tool_call(
        &mut self,
        mut call: Value,
        out: &mut VecDeque<ModelEvent>,
    ) -> Result<(), Verdict> {
        // 修 bug（修复线 F101，model.md §10.3）：以前没有 `index` 的调用让整个请求报协议错误；有的兼容端点不给
        // `index`，每个 chunk 带一个完整的调用。现在当成一个新调用（Node 用的 SDK 同样如此）。
        let key = call
            .get("index")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| self.blocks.next_tool_key());
        let mut function = call
            .get_mut("function")
            .map(Value::take)
            .unwrap_or_default();
        if !self.blocks.has_tool(key) {
            // 少数兼容网关不给调用 ID：用本次响应内唯一的占位 ID，工具结果按它回填。
            let id = take_str(&mut call, "id")
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| format!("call_{key}"));
            let name = take_str(&mut function, "name").unwrap_or_default();
            self.blocks.tool_start(key, id, name, out)?;
        }
        let arguments = take_str(&mut function, "arguments").unwrap_or_default();
        self.blocks.tool_arguments(key, arguments, out)
    }
}

impl StreamDecoder for ChatStream {
    fn decode(&mut self, event: SseEvent, out: &mut VecDeque<ModelEvent>) -> Result<(), Verdict> {
        if self.finished {
            return Ok(());
        }
        if event.data.trim() == "[DONE]" {
            if !self.started {
                return Err(protocol("stream ended before any chunk"));
            }
            self.finished = true;
            // 有函数调用却没有给出原因时按工具调用结束（与 Node 的 `tool-calls` 推断一致）。
            let stop = self
                .stop_reason
                .or_else(|| self.blocks.has_tools().then_some(StopReason::ToolUse));
            self.blocks.finish(stop, self.usage, out);
            return Ok(());
        }
        let chunk: Value = serde_json::from_str(&event.data)
            .map_err(|error| protocol(format!("invalid chunk JSON: {error}")))?;
        self.chunk(chunk, out)
    }

    fn finished(&self) -> bool {
        self.finished
    }
}

#[cfg(test)]
#[path = "openai_chat_stream_tests.rs"]
mod tests;
