//! 本工程写的合成消息（attachment、meta、中断、bash 模式的输出）的标记与读回（docs/specs/rust-cli/node-store.md §4.5）。
//! 写入（按会话的行写 message 与 part）在 node-store 的 `notices`。

use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::{
    ATTACHMENT_BLOCKS, AttachmentMessage, ContentBlock, Message, ToolResultContent, UserMessage,
    UserOrigin,
};
use serde_json::{Value, json};

use crate::keys;
use crate::records::RUST_CLI;
use crate::row::WithParts;

/// 分叉提示的来源（Node `SyntheticUserMessageSource` 的 `fork`）。
pub const FORK_NOTICE_SOURCE: &str = "fork";

/// 中断消息的 `metadata.source`：不在 Node 的提醒表里，Node 按原文发给模型。
pub const INTERRUPT_SOURCE: &str = "interrupt";

/// bash 模式输出消息的 `metadata.source`：`rust-cli/bash_output`（本工程的来源名，加前缀不与 Node 的提醒表重名）。
pub const BASH_OUTPUT_SOURCE: &str = "bash_output";

/// 与 Node 提醒表（`system-reminder/source.ts`）重名、语义不同的 attachment 来源：存成 `rust-cli/<来源>`，
/// 冷读取去掉前缀。修复线 F2：Node 把同名的当成自己的提醒（`plan_mode_exit` 在 Node 是只在运行时的提醒）。
pub const RENAMED_SOURCES: [&str; 1] = [rust_cli_kernel_api::plan::EXIT_SOURCE];

pub const ATTACHMENT: &str = "attachment";
pub const META: &str = "meta";
pub const INTERRUPT: &str = "interrupt";
pub const BASH_OUTPUT: &str = "bash_output";

/// 本工程写的合成消息还原成内核消息；没有 `metadata.rustCli` 的（Node 自己的提醒）返回 None。
pub fn hydrate(record: &WithParts) -> Option<Message> {
    let parts: Vec<&Value> = record
        .parts
        .iter()
        .filter(|part| part["type"] == "text" && part["ignored"] != true)
        .collect();
    let first = parts.first()?;
    let marker = &first["metadata"][RUST_CLI];
    let stored = first["metadata"]["source"].as_str().unwrap_or("");
    let source = stored
        .strip_prefix(keys::PREFIX)
        .filter(|name| RENAMED_SOURCES.contains(name))
        .unwrap_or(stored)
        .to_owned();
    let text = |part: &Value| part["text"].as_str().unwrap_or("").to_owned();
    let content = || {
        parts
            .iter()
            .map(|part| ContentBlock::Text { text: text(part) })
            .collect()
    };
    let id = MessageId::new(record.id());
    // Node 的分叉提示（本工程写的形状相同，没有 `rustCli`）：Node 把它当对话中间的 system 提醒
    // （`rewind_notice`）发给模型，这里还原成同来源的 attachment（session-lifecycle.md §6.5.2）。
    if marker.is_null() && source == FORK_NOTICE_SOURCE && first["synthetic"] == true {
        return Some(Message::Attachment(AttachmentMessage {
            id,
            source,
            text: rust_cli_base::reminder::wrap(&text(first)),
            data: Value::Null,
        }));
    }
    let message = match marker["message"].as_str()? {
        ATTACHMENT => Message::Attachment(AttachmentMessage {
            id,
            source,
            text: text(first),
            data: with_blocks(
                marker.get("data").cloned().unwrap_or(Value::Null),
                attachment_blocks(record),
            ),
        }),
        META => Message::User(UserMessage {
            id,
            content: content(),
            origin: UserOrigin::Meta { source },
        }),
        INTERRUPT | BASH_OUTPUT => Message::User(UserMessage {
            id,
            content: content(),
            origin: UserOrigin::Human,
        }),
        _ => return None,
    };
    Some(message)
}

/// attachment 第一个 text part 之后的 part（未忽略的 text part 与 file part）按顺序还原成内容块。
fn attachment_blocks(record: &WithParts) -> Vec<ContentBlock> {
    record
        .parts
        .iter()
        .filter(|part| {
            part["type"] == "file" || (part["type"] == "text" && part["ignored"] != true)
        })
        .skip(1)
        .map(|part| {
            if part["type"] == "text" {
                return ContentBlock::Text {
                    text: part["text"].as_str().unwrap_or("").to_owned(),
                };
            }
            match crate::tool_content::from_file_part(part) {
                ToolResultContent::Image { source } => ContentBlock::Image { source },
                ToolResultContent::Text { text } => ContentBlock::Text { text },
                // attachment 不带 document（提及不附 PDF 内容，tui-input.md §1.3）；万一有，读成 Node 的占位。
                ToolResultContent::Document { source } => ContentBlock::Text {
                    text: crate::tool_content::attached(&source),
                },
                ToolResultContent::ToolReference { tool_name } => ContentBlock::Text {
                    text: rust_cli_kernel_api::message::tool_reference_text(&tool_name),
                },
            }
        })
        .collect()
}

fn with_blocks(mut data: Value, blocks: Vec<ContentBlock>) -> Value {
    if blocks.is_empty() {
        return data;
    }
    if data.is_null() {
        data = json!({});
    }
    if let (Some(fields), Ok(blocks)) = (data.as_object_mut(), serde_json::to_value(blocks)) {
        fields.insert(ATTACHMENT_BLOCKS.to_owned(), blocks);
    }
    data
}
