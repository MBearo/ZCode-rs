//! 把模型流事件组装成一条 assistant 消息（在 `content_block_stop` 时产出完整块）。

use rust_cli_kernel_api::message::{ContentBlock, StopReason, Usage};
use rust_cli_kernel_api::model::{BlockDelta, ModelError, ModelEvent};
use rust_cli_kernel_api::unparsed_input;
use serde_json::Value;

/// 一个正在生成的块；tool_use 的输入在结束时才解析。
enum Partial {
    Done(ContentBlock),
    Text(String),
    Thinking {
        thinking: String,
        signature: Option<String>,
    },
    ToolUse {
        block: ContentBlock,
        json: String,
    },
    Other(ContentBlock),
}

/// 流结束后的结果。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Assembled {
    pub model: String,
    pub content: Vec<ContentBlock>,
    pub stop_reason: Option<StopReason>,
    pub usage: Usage,
}

#[derive(Default)]
pub(crate) struct Assembler {
    model: String,
    blocks: Vec<Option<Partial>>,
    stop_reason: Option<StopReason>,
    usage: Usage,
    stopped: bool,
}

fn protocol(message: impl Into<String>) -> ModelError {
    ModelError::Protocol {
        message: message.into(),
    }
}

impl Assembler {
    /// 处理一个事件；一个 tool_use 块结束时返回这个块，调用方可以马上开始执行它。
    pub(crate) fn push(&mut self, event: ModelEvent) -> Result<Option<ContentBlock>, ModelError> {
        match event {
            ModelEvent::MessageStart { model, usage, .. } => {
                self.model = model;
                self.usage = usage;
            }
            ModelEvent::BlockStart { index, block } => {
                if self.blocks.len() <= index {
                    self.blocks.resize_with(index + 1, || None);
                }
                self.blocks[index] = Some(match block {
                    ContentBlock::Text { text } => Partial::Text(text),
                    ContentBlock::Thinking {
                        thinking,
                        signature,
                    } => Partial::Thinking {
                        thinking,
                        signature,
                    },
                    block @ ContentBlock::ToolUse { .. } => Partial::ToolUse {
                        block,
                        json: String::new(),
                    },
                    other => Partial::Other(other),
                });
            }
            ModelEvent::BlockDelta { index, delta } => {
                let partial = self
                    .blocks
                    .get_mut(index)
                    .and_then(Option::as_mut)
                    .ok_or_else(|| protocol(format!("delta for unknown block {index}")))?;
                match (partial, delta) {
                    (Partial::Text(text), BlockDelta::Text(more)) => text.push_str(&more),
                    (Partial::Thinking { thinking, .. }, BlockDelta::Thinking(more)) => {
                        thinking.push_str(&more);
                    }
                    (Partial::Thinking { signature, .. }, BlockDelta::Signature(value)) => {
                        signature.get_or_insert_with(String::new).push_str(&value);
                    }
                    (Partial::ToolUse { json, .. }, BlockDelta::InputJson(more)) => {
                        json.push_str(&more);
                    }
                    _ => return Err(protocol(format!("delta does not match block {index}"))),
                }
            }
            ModelEvent::BlockStop { index } => {
                let slot = self
                    .blocks
                    .get_mut(index)
                    .ok_or_else(|| protocol(format!("stop for unknown block {index}")))?;
                let finished = match slot.take() {
                    Some(Partial::Text(text)) => ContentBlock::Text { text },
                    Some(Partial::Thinking {
                        thinking,
                        signature,
                    }) => ContentBlock::Thinking {
                        thinking,
                        signature,
                    },
                    Some(Partial::ToolUse { block, json }) => finish_tool_use(block, &json),
                    Some(Partial::Other(block) | Partial::Done(block)) => block,
                    None => return Err(protocol(format!("block {index} stopped twice"))),
                };
                let tool_use =
                    matches!(finished, ContentBlock::ToolUse { .. }).then(|| finished.clone());
                *slot = Some(Partial::Done(finished));
                return Ok(tool_use);
            }
            ModelEvent::MessageDelta { stop_reason, usage } => {
                self.stop_reason = stop_reason.or(self.stop_reason);
                self.usage.output_tokens = usage.output_tokens;
            }
            ModelEvent::MessageStop => self.stopped = true,
        }
        Ok(None)
    }

    /// 流没有正常收尾（没有 `message_stop`，或还有没结束的块）时的错误。
    pub(crate) fn incomplete(&self) -> Option<ModelError> {
        if !self.stopped {
            return Some(protocol("stream ended before message_stop"));
        }
        self.blocks
            .iter()
            .any(|block| !matches!(block, Some(Partial::Done(_))))
            .then(|| protocol("stream ended with an unfinished block"))
    }

    /// 只保留已经结束的块（块结束时才产出消息，未结束的块不进历史）。中断、出错与正常结束
    /// 都用它；正常结束时调用方先用 [`Self::incomplete`] 确认全部块都已结束。
    ///
    /// 缺陷修复（R5-1）：原来正常结束走另一个返回 `Result` 的 `finish`，出错时调用方用 `?` 提前返回，已经开工的
    /// 工具被丢下、assistant 消息与计量都不落库。现在错误由调用方并入流后已有的“等工具、提交”路径。
    pub(crate) fn finish(self) -> Assembled {
        let content = self
            .blocks
            .into_iter()
            .filter_map(|block| match block {
                Some(Partial::Done(block)) => Some(block),
                _ => None,
            })
            .collect();
        Assembled {
            model: self.model,
            content,
            stop_reason: self.stop_reason,
            usage: self.usage,
        }
    }
}

impl Assembler {
    /// 主线程被中止时的结果（修复线 F38，query.md §1.4）：已经结束的块，
    /// 再加第一个没结束的块——只在它是文字、`trim` 后不为空时（thinking、tool_use 等没结束的块不留）。
    pub(crate) fn finish_aborted(mut self) -> Assembled {
        let open = self
            .blocks
            .iter()
            .position(|block| !matches!(block, Some(Partial::Done(_))));
        if let Some(index) = open {
            let kept = match self.blocks[index].take() {
                Some(Partial::Text(text)) if !text.trim().is_empty() => {
                    Some(Partial::Done(ContentBlock::Text { text }))
                }
                _ => None,
            };
            self.blocks.truncate(index);
            self.blocks.push(kept);
        }
        self.finish()
    }
}

/// 空输入按 `{}` 处理（Anthropic 对无参数工具只发空串）；字面量 `null` 同样是 `{}`。
///
/// 缺陷修复（R5-1）：原来输入不是合法 JSON 时返回协议错误，整个回合失败——同一响应里已经开工的工具被丢下
/// （Bash 仍写出副作用），库里只剩意图。应把它换成标记对象，回合继续，执行管线在 schema 校验前给模型
/// JSON 解析失败的 InputValidationError（tools 的 `pipeline`）。这里换成标记对象，不再失败。
fn finish_tool_use(block: ContentBlock, json: &str) -> ContentBlock {
    let ContentBlock::ToolUse { id, name, .. } = block else {
        return block;
    };
    let input = if json.trim().is_empty() {
        Value::Object(serde_json::Map::new())
    } else {
        match serde_json::from_str(json) {
            Ok(Value::Null) => Value::Object(serde_json::Map::new()),
            Ok(input) => input,
            Err(_) => unparsed_input::marker(json),
        }
    };
    ContentBlock::ToolUse { id, name, input }
}

#[cfg(test)]
#[path = "assembler_tests.rs"]
mod tests;
