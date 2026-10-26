//! OpenAI 两种流的公共部分：把文本、推理与函数调用的增量排成 Anthropic 形状的块事件
//! （docs/specs/rust-cli/model.md §10.3）。
//!
//! - 正文与推理是“当前流”：同一时刻只开一个，换种类时先结束前一个块；
//! - 每个函数调用一个 tool_use 块，按协议给的键（Chat 的 `index`、Responses 的 `output_index`）对应，
//!   开到协议说它结束（Responses）或整条消息结束（Chat）；
//! - 块号按打开的先后从 0 递增，与 Anthropic 的 `index` 含义相同。
//!
//! 推理块的 signature 为空（`None`）：OpenAI 协议没有 Anthropic 的 thinking 签名。

use std::collections::VecDeque;

use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::message::{ContentBlock, StopReason, Usage};
use rust_cli_kernel_api::model::{BlockDelta, ModelError, ModelEvent};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flow {
    Text,
    Thinking,
}

#[derive(Debug)]
struct Tool {
    key: u64,
    index: usize,
    open: bool,
}

#[derive(Debug, Default)]
pub(crate) struct Blocks {
    next: usize,
    current: Option<(usize, Flow)>,
    tools: Vec<Tool>,
}

pub(crate) fn protocol(message: impl Into<String>) -> ModelError {
    ModelError::Protocol {
        message: message.into(),
    }
}

impl Blocks {
    fn open(&mut self, block: ContentBlock, out: &mut VecDeque<ModelEvent>) -> usize {
        let index = self.next;
        self.next += 1;
        out.push_back(ModelEvent::BlockStart { index, block });
        index
    }

    /// 正文或推理的一段增量；空串不输出。
    pub fn flow(&mut self, flow: Flow, text: String, out: &mut VecDeque<ModelEvent>) {
        if text.is_empty() {
            return;
        }
        let index = match self.current {
            Some((index, current)) if current == flow => index,
            _ => {
                self.end_flow(out);
                let block = match flow {
                    Flow::Text => ContentBlock::Text {
                        text: String::new(),
                    },
                    Flow::Thinking => ContentBlock::Thinking {
                        thinking: String::new(),
                        signature: None,
                    },
                };
                let index = self.open(block, out);
                self.current = Some((index, flow));
                index
            }
        };
        let delta = match flow {
            Flow::Text => BlockDelta::Text(text),
            Flow::Thinking => BlockDelta::Thinking(text),
        };
        out.push_back(ModelEvent::BlockDelta { index, delta });
    }

    /// 结束当前的正文或推理块。
    pub fn end_flow(&mut self, out: &mut VecDeque<ModelEvent>) {
        if let Some((index, _)) = self.current.take() {
            out.push_back(ModelEvent::BlockStop { index });
        }
    }

    pub fn has_tool(&self, key: u64) -> bool {
        self.tools.iter().any(|tool| tool.key == key)
    }

    pub fn has_tools(&self) -> bool {
        !self.tools.is_empty()
    }

    /// 没有给键的调用用的新键：已有的最大键 + 1，没有调用时是 0。
    pub fn next_tool_key(&self) -> u64 {
        self.tools
            .iter()
            .map(|tool| tool.key + 1)
            .max()
            .unwrap_or(0)
    }

    /// 打开一个函数调用块；同一个键只能打开一次。
    pub fn tool_start(
        &mut self,
        key: u64,
        id: String,
        name: String,
        out: &mut VecDeque<ModelEvent>,
    ) -> Result<(), ModelError> {
        if self.has_tool(key) {
            return Err(protocol(format!("function call {key} started twice")));
        }
        if name.is_empty() {
            return Err(protocol(format!("function call {key} has no name")));
        }
        self.end_flow(out);
        let block = ContentBlock::ToolUse {
            id: ToolUseId::new(id),
            name,
            input: Value::Object(Map::new()),
        };
        let index = self.open(block, out);
        self.tools.push(Tool {
            key,
            index,
            open: true,
        });
        Ok(())
    }

    /// 函数调用参数的一段增量（JSON 文本片段）。
    pub fn tool_arguments(
        &mut self,
        key: u64,
        arguments: String,
        out: &mut VecDeque<ModelEvent>,
    ) -> Result<(), ModelError> {
        let tool = self
            .tools
            .iter()
            .find(|tool| tool.key == key && tool.open)
            .ok_or_else(|| protocol(format!("arguments for unknown function call {key}")))?;
        if !arguments.is_empty() {
            out.push_back(ModelEvent::BlockDelta {
                index: tool.index,
                delta: BlockDelta::InputJson(arguments),
            });
        }
        Ok(())
    }

    /// 结束一个函数调用块。
    pub fn tool_stop(&mut self, key: u64, out: &mut VecDeque<ModelEvent>) {
        if let Some(tool) = self
            .tools
            .iter_mut()
            .find(|tool| tool.key == key && tool.open)
        {
            tool.open = false;
            out.push_back(ModelEvent::BlockStop { index: tool.index });
        }
    }

    /// 消息结束：结束还开着的块，再输出停止原因与用量。
    pub fn finish(
        &mut self,
        stop_reason: Option<StopReason>,
        usage: Usage,
        out: &mut VecDeque<ModelEvent>,
    ) {
        self.end_flow(out);
        for tool in self.tools.iter_mut().filter(|tool| tool.open) {
            tool.open = false;
            out.push_back(ModelEvent::BlockStop { index: tool.index });
        }
        out.push_back(ModelEvent::MessageDelta { stop_reason, usage });
        out.push_back(ModelEvent::MessageStop);
    }
}

/// OpenAI 用量换成 Anthropic 的口径：`input_tokens` 不含缓存命中的部分。
pub(crate) fn usage(input: u64, cached: u64, output: u64) -> Usage {
    Usage {
        input_tokens: input.saturating_sub(cached),
        output_tokens: output,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: cached,
    }
}

/// 字段是字符串时移出，否则为空。
pub(crate) fn take_str(value: &mut Value, name: &str) -> Option<String> {
    match value.get_mut(name).map(Value::take) {
        Some(Value::String(text)) => Some(text),
        _ => None,
    }
}

pub(crate) fn number(value: &Value, name: &str) -> u64 {
    value.get(name).and_then(Value::as_u64).unwrap_or(0)
}
