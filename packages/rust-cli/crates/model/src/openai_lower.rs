//! canonical 请求（Anthropic Messages 形状）到 OpenAI 两种协议的公共降级（docs/specs/rust-cli/model.md §10）。
//!
//! 先把消息降成与协议无关的 [`Item`] 序列，再由 Chat / Responses 各自写成线格式：
//!
//! | canonical                               | Item                                                               |
//! | --------------------------------------- | ------------------------------------------------------------------ |
//! | `system` 块                             | 一个 [`Item::System`]，块之间用空行连接；`cache_control` 丢弃      |
//! | 对话中间的 `role: system` 消息          | user 内容里的 `<system-reminder>` 文本（关掉 beta 时的形状），挪到下一条 user 的工具结果之后 |
//! | user 的 text / image                    | [`Item::User`] 的文本 / 图片片段，相邻的 user 合并                 |
//! | user 的 tool_result                     | [`Item::ToolResult`]（文本连接）；结果里的图片与 PDF 放进紧随的 user 片段 |
//! | assistant 的 text / tool_use            | [`Item::Assistant`] 的正文与函数调用（参数是 JSON 文本）          |
//! | assistant 的 thinking                   | 正文进 `reasoning`；签名与 redacted_thinking 丢弃                  |
//! | 块上的 `cache_control`、消息级 `output_config` | 丢弃                                                        |

use rust_cli_base::js::json::stringify;
use rust_cli_kernel_api::message::{
    ContentBlock, ImageSource, ToolResultContent, tool_reference_text,
};
use rust_cli_kernel_api::model::{ApiMessage, ApiRole, ModelError, SystemBlock};

/// 一个用户片段。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Part {
    Text(String),
    /// `data:<media_type>;base64,<data>`。
    Image(String),
    /// PDF 文档（Read 整读的 document 块，model.md §10.6）：`data:application/pdf;base64,<data>`。
    File(String),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Call {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Item {
    System(String),
    User(Vec<Part>),
    Assistant {
        text: String,
        reasoning: String,
        calls: Vec<Call>,
    },
    ToolResult {
        call_id: String,
        output: String,
    },
}

/// 系统提示：各块文本用空行连接；没有文本时为空。
pub(crate) fn system_prompt(system: Vec<SystemBlock>) -> Option<String> {
    let text = system
        .into_iter()
        .map(|block| block.text)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    (!text.is_empty()).then_some(text)
}

/// 消息降级成 Item。消费消息：文本移入，不复制。
pub(crate) fn items(
    system: Vec<SystemBlock>,
    messages: Vec<ApiMessage>,
) -> Result<Vec<Item>, ModelError> {
    let mut out = Vec::new();
    if let Some(prompt) = system_prompt(system) {
        out.push(Item::System(prompt));
    }
    // 对话中间的 system 消息降成提醒后，等到下一条 user 消息（排在它的工具结果之后）或下一条 assistant
    // 之前再写出：OpenAI 要求工具结果紧跟发出调用的 assistant 消息。
    let mut reminders = Vec::new();
    for message in messages {
        match message.role {
            ApiRole::User => user(
                &mut out,
                message.content.into_iter().map(|c| c.block),
                &mut reminders,
            )?,
            ApiRole::System => {
                reminders.extend(message.content.into_iter().filter_map(|content| {
                    match content.block {
                        ContentBlock::Text { text } => {
                            Some(Part::Text(rust_cli_base::reminder::wrap(&text)))
                        }
                        _ => None,
                    }
                }));
            }
            ApiRole::Assistant => {
                push_user(&mut out, std::mem::take(&mut reminders));
                let (mut text, mut reasoning, mut calls) = (String::new(), String::new(), vec![]);
                for content in message.content {
                    match content.block {
                        ContentBlock::Text { text: more } => text.push_str(&more),
                        ContentBlock::Thinking { thinking, .. } => reasoning.push_str(&thinking),
                        ContentBlock::ToolUse { id, name, input } => calls.push(Call {
                            id: id.as_str().to_owned(),
                            name,
                            arguments: stringify(&input),
                        }),
                        // 服务端工具的块在 OpenAI 协议里没有对应物，不回放（model.md §10）。
                        ContentBlock::RedactedThinking { .. }
                        | ContentBlock::ServerToolUse { .. }
                        | ContentBlock::WebSearchToolResult { .. } => {}
                        ContentBlock::Image { .. } | ContentBlock::ToolResult { .. } => {
                            return Err(invalid(
                                "assistant messages carry only text and tool calls",
                            ));
                        }
                    }
                }
                out.push(Item::Assistant {
                    text,
                    reasoning,
                    calls,
                });
            }
        }
    }
    push_user(&mut out, reminders);
    Ok(out)
}

/// 一条 user 消息：工具结果按顺序先写出，再写之前积下的提醒，最后是这条消息的其余片段。
fn user(
    out: &mut Vec<Item>,
    blocks: impl Iterator<Item = ContentBlock>,
    reminders: &mut Vec<Part>,
) -> Result<(), ModelError> {
    let mut images = Vec::new();
    let mut parts = Vec::new();
    for block in blocks {
        match block {
            ContentBlock::Text { text } => parts.push(Part::Text(text)),
            ContentBlock::Image { source } => parts.push(Part::Image(data_url(source)?)),
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } => {
                // 工具消息只能是文本：结果里的图片放进紧随的 user 片段（Node 的 tool_media 同样处理）。
                let mut texts = Vec::new();
                for item in content {
                    match item {
                        ToolResultContent::Text { text } => texts.push(text),
                        ToolResultContent::Image { source } => {
                            images.push(Part::Image(data_url(source)?));
                        }
                        ToolResultContent::Document { source } => {
                            images.push(Part::File(data_url(source)?));
                        }
                        // OpenAI 没有 tool_reference：降成文本（model.md §10）。
                        ToolResultContent::ToolReference { tool_name } => {
                            texts.push(tool_reference_text(&tool_name));
                        }
                    }
                }
                out.push(Item::ToolResult {
                    call_id: tool_use_id.as_str().to_owned(),
                    output: texts.join("\n"),
                });
            }
            ContentBlock::Thinking { .. }
            | ContentBlock::RedactedThinking { .. }
            | ContentBlock::ServerToolUse { .. }
            | ContentBlock::WebSearchToolResult { .. } => {}
            ContentBlock::ToolUse { .. } => {
                return Err(invalid("user messages cannot carry tool calls"));
            }
        }
    }
    let mut merged = std::mem::take(reminders);
    merged.extend(images);
    merged.extend(parts);
    push_user(out, merged);
    Ok(())
}

/// 追加 user 片段，与前一个 user 合并。
fn push_user(out: &mut Vec<Item>, parts: Vec<Part>) {
    if parts.is_empty() {
        return;
    }
    match out.last_mut() {
        Some(Item::User(existing)) => existing.extend(parts),
        _ => out.push(Item::User(parts)),
    }
}

fn data_url(source: ImageSource) -> Result<String, ModelError> {
    match source {
        ImageSource::Base64 { media_type, data } => Ok(format!("data:{media_type};base64,{data}")),
        ImageSource::Artifact { .. } => Err(invalid(
            "image artifacts must be materialized before the model request",
        )),
    }
}

pub(crate) fn invalid(message: &str) -> ModelError {
    ModelError::InvalidRequest {
        message: message.to_owned(),
    }
}

#[cfg(test)]
#[path = "openai_lower_tests.rs"]
pub(crate) mod tests;
