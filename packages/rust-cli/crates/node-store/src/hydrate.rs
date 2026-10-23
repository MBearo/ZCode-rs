//! Node 记录 → 内核消息（docs/specs/rust-cli/node-store.md §5）。
//!
//! 规则取自 Node `hydrateMessageHistoryFromSession` 的文本子集（复制来源
//! `apps/zcode-cli-rust/crates/domain/src/node_history/hydrate.rs`）：
//! - 本工程写的合成消息（attachment、meta、中断）按 `metadata.rustCli` 还原（[`crate::notices`]）；
//! - user：未忽略的非合成 text part，`metadata.modelInput` 优先；压缩摘要（`summary`）连合成 part
//!   一起读；Node 自己的提醒（只有合成 part）、未接入的共享上下文跳过。
//! - assistant：reasoning part → thinking 块（`metadata.anthropic.signature` / `redactedData`），
//!   未忽略的非空 text part → text 块，tool part → tool_use 块；没有任何块时跳过。已结束的 tool part
//!   拆成紧跟其后的工具结果消息（[`crate::tool_parts`]）。
//! - 文件、agent、时间线等 part 不读。
use std::borrow::Cow;

use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, StopReason, ToolResultContent, Usage, UserMessage,
    UserOrigin,
};
use serde_json::Value;

use crate::branch::truthy;
use crate::repo::messages::WithParts;
use crate::tool_parts;

pub fn hydrate(active: &[Cow<'_, WithParts>]) -> Vec<Message> {
    let mut out = Vec::with_capacity(active.len());
    for record in active {
        match record.info["role"].as_str() {
            Some("user") => {
                // 压缩摘要之前还原边界（docs/specs/rust-cli/compact.md §6）。
                if truthy(record.info.get("summary")) {
                    out.extend(crate::compaction::hydrate_boundary(
                        &record.parts,
                        record.id(),
                    ));
                }
                out.extend(crate::notices::hydrate(record).or_else(|| user(record)));
            }
            Some("assistant") => {
                let tools = tool_parts::selected(&record.parts);
                out.extend(assistant(record, &tools));
                out.extend(tool_parts::results(&MessageId::new(record.id()), &tools));
            }
            _ => {}
        }
    }
    out
}

fn visible_text(part: &Value) -> bool {
    part["type"] == "text" && part["ignored"] != true
}

/// Node `textPartToProviderText` 的非合成分支：`/init` 与自定义命令的模型输入在 `metadata.modelInput`。
fn provider_text(part: &Value) -> &str {
    if part["synthetic"] != true
        && let Some(input) = part["metadata"]["modelInput"].as_str()
    {
        return input;
    }
    part["text"].as_str().unwrap_or("")
}

fn user(record: &WithParts) -> Option<Message> {
    let info = &record.info;
    let shared = &info["metadata"]["sharedContextStatus"];
    if info["source"] == "shared_context" && !shared.is_null() && *shared != "attached" {
        // 共享上下文在首次发送前只是候选，不能提前进入模型上下文。
        return None;
    }
    let summary = truthy(info.get("summary"));
    let content: Vec<ContentBlock> = record
        .parts
        .iter()
        .filter_map(|part| {
            // 人类输入的图片（Node `file` part，tui-input.md §3.2）按 part 的顺序读回；其他附件与读不出的
            // 图片仍然不读（node-store.md §5）。
            if part["type"] == "file" {
                return match crate::tool_content::from_file_part(part) {
                    ToolResultContent::Image { source } if !summary => {
                        Some(ContentBlock::Image { source })
                    }
                    _ => None,
                };
            }
            if !(visible_text(part) && (summary || part["synthetic"] != true)) {
                return None;
            }
            let text = provider_text(part);
            (!text.is_empty()).then(|| ContentBlock::Text {
                text: text.to_owned(),
            })
        })
        .collect();
    let has_content = content.iter().any(|block| match block {
        ContentBlock::Text { text } => !rust_cli_base::js::string::trim(text).is_empty(),
        _ => true,
    });
    if !has_content {
        return None;
    }
    Some(Message::User(UserMessage {
        id: MessageId::new(record.id()),
        content,
        origin: if summary {
            UserOrigin::CompactSummary
        } else {
            UserOrigin::Human
        },
    }))
}

fn thinking(part: &Value) -> ContentBlock {
    let anthropic = &part["metadata"]["anthropic"];
    if let Some(data) = anthropic["redactedData"].as_str() {
        return ContentBlock::RedactedThinking {
            data: data.to_owned(),
        };
    }
    ContentBlock::Thinking {
        thinking: part["text"].as_str().unwrap_or("").to_owned(),
        signature: anthropic["signature"].as_str().map(str::to_owned),
    }
}

/// assistant：推理与文本块按 part 顺序，tool_use 块排在后面（Node `addAssistant` 把工具调用单独放在
/// 文本之后）。
fn assistant(record: &WithParts, tools: &[&Value]) -> Option<Message> {
    let mut content: Vec<ContentBlock> = record
        .parts
        .iter()
        .filter_map(|part| match part["type"].as_str() {
            Some("reasoning") => Some(thinking(part)),
            Some("text") if visible_text(part) => part["text"]
                .as_str()
                .filter(|text| !text.is_empty())
                .map(|text| ContentBlock::Text {
                    text: text.to_owned(),
                }),
            _ => None,
        })
        .collect();
    content.extend(tools.iter().map(|part| tool_parts::tool_use(part)));
    if content.is_empty() {
        return None;
    }
    let info = &record.info;
    Some(Message::Assistant(AssistantMessage {
        id: MessageId::new(record.id()),
        content,
        model: info["modelId"].as_str().unwrap_or("").to_owned(),
        stop_reason: info["finish"].as_str().map(stop_reason),
        usage: usage(&info["tokens"]),
    }))
}

/// AI SDK `finishReason` → Anthropic `stop_reason`（`stop` 读回 end_turn，见 spec 的已知差异）。
fn stop_reason(finish: &str) -> StopReason {
    match finish {
        "stop" => StopReason::EndTurn,
        "length" => StopReason::MaxTokens,
        "tool-calls" => StopReason::ToolUse,
        "content-filter" => StopReason::Refusal,
        _ => StopReason::Unknown,
    }
}

/// Node `tokens` → Anthropic `usage`：`input` 含缓存读写，拆回不含缓存的 `input_tokens`。全为 0
/// （Node `emptyTokenUsageInfo`）时没有用量。
fn usage(tokens: &Value) -> Option<Usage> {
    let count = |value: &Value| value.as_u64().unwrap_or(0);
    let input = count(&tokens["input"]);
    let read = count(&tokens["cache"]["read"]);
    let write = count(&tokens["cache"]["write"]);
    let output = count(&tokens["output"]);
    if input == 0 && read == 0 && write == 0 && output == 0 {
        return None;
    }
    Some(Usage {
        input_tokens: input.saturating_sub(read.saturating_add(write)),
        output_tokens: output,
        cache_creation_input_tokens: write,
        cache_read_input_tokens: read,
    })
}
