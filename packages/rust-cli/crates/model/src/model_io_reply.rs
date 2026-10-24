//! 模型轨迹里 Node 形状的回复字段（工作线 F93，docs/specs/rust-cli/model-io.md §4.2）。
//!
//! App 的模型轨迹面板（`readModelTrajectory`）读 `response.text`、`reasoningText`、`toolCalls`、`finishReason` 与
//! `usage`，形状照 Node 的 `runner-debug.ts`。原料由包住解码器的 Tape 在请求路径上收（只把增量接到各自的块上）；拼参数
//! JSON、换算停止原因与用量在写者线程上做（[`Reply::write`]）。

use rust_cli_kernel_api::message::{ContentBlock, StopReason, Usage};
use rust_cli_kernel_api::model::BlockDelta;
use serde_json::{Map, Value, json};

/// 一个块收到的内容。
#[derive(Debug, Clone)]
enum Part {
    Text(String),
    Thinking(String),
    /// 调用：开始事件里的输入与拼起来的参数 JSON；`server` 是服务端执行的调用；`done` 是收到了结束事件。
    Call {
        id: String,
        name: String,
        start: Value,
        json: String,
        server: bool,
        done: bool,
    },
}

/// 一次回复里各块收到的内容（按开始的先后，写出时按块序号排）。
#[derive(Debug, Default, Clone)]
pub(crate) struct Reply {
    parts: Vec<(usize, Part)>,
}

impl Reply {
    fn part(&mut self, index: usize) -> Option<&mut Part> {
        self.parts
            .iter_mut()
            .find(|(at, _)| *at == index)
            .map(|(_, part)| part)
    }

    /// 解码器产出的块开始。
    pub(crate) fn start(&mut self, index: usize, block: &ContentBlock) {
        let part = match block {
            ContentBlock::Text { text } => Part::Text(text.clone()),
            ContentBlock::Thinking { thinking, .. } => Part::Thinking(thinking.clone()),
            ContentBlock::ToolUse { id, name, input } => Part::Call {
                id: id.as_str().to_owned(),
                name: name.clone(),
                start: input.clone(),
                json: String::new(),
                server: false,
                done: false,
            },
            ContentBlock::ServerToolUse { id, name, input } => Part::Call {
                id: id.clone(),
                name: name.clone(),
                start: input.clone(),
                json: String::new(),
                server: true,
                done: false,
            },
            _ => return,
        };
        self.parts.push((index, part));
    }

    /// 解码器产出的增量（签名不记）。
    pub(crate) fn delta(&mut self, index: usize, delta: &BlockDelta) {
        match (self.part(index), delta) {
            (Some(Part::Text(text)), BlockDelta::Text(more))
            | (Some(Part::Thinking(text)), BlockDelta::Thinking(more))
            | (Some(Part::Call { json: text, .. }), BlockDelta::InputJson(more)) => {
                text.push_str(more);
            }
            _ => {}
        }
    }

    /// 解码器产出的块结束。
    pub(crate) fn stop(&mut self, index: usize) {
        if let Some(Part::Call { done, .. }) = self.part(index) {
            *done = true;
        }
    }

    /// 有没有服务端调用（它的参数增量解码器不输出，要从原始事件里取，见 [`Reply::server_input`]）。
    pub(crate) fn has_server_call(&self) -> bool {
        self.parts
            .iter()
            .any(|(_, part)| matches!(part, Part::Call { server: true, .. }))
    }

    /// 服务端调用的参数增量（原始事件的 `input_json_delta`）。不是服务端调用的序号不理（本地调用的增量经解码器来）。
    pub(crate) fn server_input(&mut self, index: usize, partial: &str) {
        if let Some(Part::Call {
            json, server: true, ..
        }) = self.part(index)
        {
            json.push_str(partial);
        }
    }

    /// 写出 `text`、`reasoningText`（没有时不写）与 `toolCalls`（Node `recordStreamTextDebug` 的同名字段）。
    pub(crate) fn write(mut self, out: &mut Map<String, Value>) {
        self.parts.sort_by_key(|(index, _)| *index);
        let mut text = String::new();
        let mut reasoning: Vec<String> = Vec::new();
        let mut calls: Vec<Value> = Vec::new();
        for (_, part) in self.parts {
            match part {
                Part::Text(more) => text.push_str(&more),
                Part::Thinking(more) => {
                    if !more.trim().is_empty() {
                        reasoning.push(more);
                    }
                }
                Part::Call {
                    id,
                    name,
                    start,
                    json,
                    server,
                    done: true,
                } => {
                    let input = if json.is_empty() {
                        start
                    } else {
                        serde_json::from_str(&json).unwrap_or(Value::String(json))
                    };
                    let mut call = Map::new();
                    call.insert("id".into(), id.into());
                    call.insert("name".into(), name.into());
                    call.insert("input".into(), input);
                    if server {
                        call.insert("providerExecuted".into(), true.into());
                    }
                    calls.push(Value::Object(call));
                }
                // 没收到结束事件的调用（流中途断开）不写，同 Node 只写 assembler 收齐的调用。
                Part::Call { .. } => {}
            }
        }
        out.insert("text".into(), text.into());
        if !reasoning.is_empty() {
            out.insert("reasoningText".into(), reasoning.join("\n\n").into());
        }
        out.insert("toolCalls".into(), Value::Array(calls));
    }
}

/// 停止原因换成 AI SDK 的统一值（`@ai-sdk/anthropic` 3.0.81 的 `mapAnthropicStopReason`）。OpenAI 两种协议的停止原因
/// 解码器已换成同一组（model.md §10.3），按同一张表。
pub(crate) fn finish_reason(reason: StopReason) -> &'static str {
    match reason {
        StopReason::EndTurn | StopReason::StopSequence | StopReason::PauseTurn => "stop",
        StopReason::ToolUse => "tool-calls",
        StopReason::MaxTokens => "length",
        StopReason::Refusal => "content-filter",
        StopReason::Unknown => "other",
    }
}

/// 用量换成 Node 的键名（`convertAnthropicMessagesUsage` + `normalizeUsage`）：输入含缓存的读与写，合计是输入加输出。
/// Anthropic 没有推理 token，Node 不写 `reasoningTokens`。
pub(crate) fn usage(usage: Usage) -> Value {
    let input =
        usage.input_tokens + usage.cache_creation_input_tokens + usage.cache_read_input_tokens;
    json!({
        "inputTokens": input,
        "outputTokens": usage.output_tokens,
        "totalTokens": input + usage.output_tokens,
        "cacheReadTokens": usage.cache_read_input_tokens,
        "cacheWriteTokens": usage.cache_creation_input_tokens,
    })
}
