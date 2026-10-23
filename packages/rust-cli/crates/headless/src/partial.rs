//! `--include-partial-messages` 的 `stream_event`（docs/specs/rust-cli/headless.md §3.7）：由内核的流增量合成
//! Anthropic 的流事件。内核的增量里没有消息 ID、用量与 tool_use 的 ID / 名字，这几项在消息完成时补（D10）。

use std::collections::BTreeSet;

use rust_cli_kernel_api::message::{AssistantMessage, ContentBlock};
use rust_cli_kernel_api::model::BlockDelta;
use serde_json::{Value, json};

/// 一次响应里已经写出的流事件。
#[derive(Debug, Default)]
pub(crate) struct Partial {
    /// 已写 `message_start`。
    started: bool,
    /// 已写 `content_block_start` 的块。
    blocks: BTreeSet<usize>,
}

fn message_start(model: &str) -> Value {
    json!({
        "type": "message_start",
        "message": {
            "id": null,
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": [],
            "stop_reason": null,
            "stop_sequence": null,
            "usage": {
                "input_tokens": 0,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0,
                "output_tokens": 0,
            },
        },
    })
}

fn block_start(index: usize, block: Value) -> Value {
    json!({"type": "content_block_start", "index": index, "content_block": block})
}

fn block_delta(index: usize, delta: Value) -> Value {
    json!({"type": "content_block_delta", "index": index, "delta": delta})
}

/// 消息完成时还没开始的块：开始，再一次给出全部内容。
fn whole_block(index: usize, block: &ContentBlock) -> Vec<Value> {
    match block {
        ContentBlock::Text { text } => vec![
            block_start(index, json!({"type": "text", "text": ""})),
            block_delta(index, json!({"type": "text_delta", "text": text})),
        ],
        ContentBlock::Thinking {
            thinking,
            signature,
        } => {
            let mut out = vec![
                block_start(
                    index,
                    json!({"type": "thinking", "thinking": "", "signature": ""}),
                ),
                block_delta(
                    index,
                    json!({"type": "thinking_delta", "thinking": thinking}),
                ),
            ];
            if let Some(signature) = signature {
                out.push(block_delta(
                    index,
                    json!({"type": "signature_delta", "signature": signature}),
                ));
            }
            out
        }
        ContentBlock::ToolUse { id, name, input } => vec![
            block_start(
                index,
                json!({"type": "tool_use", "id": id.as_str(), "name": name, "input": {}}),
            ),
            block_delta(
                index,
                json!({"type": "input_json_delta", "partial_json": input.to_string()}),
            ),
        ],
        other => vec![block_start(index, crate::events::block(other))],
    }
}

impl Partial {
    /// 一个流增量对应的事件。tool_use 的输入增量不写（等消息完成）。
    pub(crate) fn delta(&mut self, model: &str, index: usize, delta: &BlockDelta) -> Vec<Value> {
        let mut out = Vec::new();
        if !self.started {
            self.started = true;
            out.push(message_start(model));
        }
        let (start, payload) = match delta {
            BlockDelta::Text(text) => (
                json!({"type": "text", "text": ""}),
                json!({"type": "text_delta", "text": text}),
            ),
            BlockDelta::Thinking(text) => (
                json!({"type": "thinking", "thinking": "", "signature": ""}),
                json!({"type": "thinking_delta", "thinking": text}),
            ),
            BlockDelta::Signature(signature) => (
                json!({"type": "thinking", "thinking": "", "signature": ""}),
                json!({"type": "signature_delta", "signature": signature}),
            ),
            BlockDelta::InputJson(_) => return out,
        };
        if self.blocks.insert(index) {
            out.push(block_start(index, start));
        }
        out.push(block_delta(index, payload));
        out
    }

    /// 消息完成：`before` 写在 assistant 事件之前（还没开始的块），`after` 写在之后（每个块的
    /// `content_block_stop`、`message_delta`、`message_stop`）。之后这次响应的状态清空。
    pub(crate) fn finish(&mut self, message: &AssistantMessage) -> (Vec<Value>, Vec<Value>) {
        let mut before = Vec::new();
        if !self.started {
            before.push(message_start(&message.model));
        }
        for (index, block) in message.content.iter().enumerate() {
            if !self.blocks.contains(&index) {
                before.extend(whole_block(index, block));
            }
        }
        let mut after: Vec<Value> = (0..message.content.len())
            .map(|index| json!({"type": "content_block_stop", "index": index}))
            .collect();
        let output_tokens = message.usage.map_or(0, |usage| usage.output_tokens);
        after.push(json!({
            "type": "message_delta",
            "delta": {"stop_reason": message.stop_reason, "stop_sequence": null},
            "usage": {"output_tokens": output_tokens},
        }));
        after.push(json!({"type": "message_stop"}));
        *self = Self::default();
        (before, after)
    }
}

/// 包成 stream-json 的一行（键序：`type`、`event`、`session_id`、`parent_tool_use_id`、`uuid`）。
pub(crate) fn wrap(event: Value, session_id: &str, uuid: &str) -> Value {
    json!({
        "type": "stream_event",
        "event": event,
        "session_id": session_id,
        "parent_tool_use_id": null,
        "uuid": uuid,
    })
}
