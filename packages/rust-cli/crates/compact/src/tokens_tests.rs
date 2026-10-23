use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, Usage, UserMessage, UserOrigin,
};

use super::*;

fn user(text: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new("u"),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        origin: UserOrigin::Human,
    })
}

#[test]
fn rough_is_a_quarter_of_utf16_units_rounded() {
    assert_eq!(rough(""), 0);
    assert_eq!(rough("ab"), 1);
    assert_eq!(rough("abcdef"), 2);
    assert_eq!(rough("abcdefgh"), 2);
    assert_eq!(rough("😀😀"), 1);
}

#[test]
fn count_anchors_on_the_last_usage() {
    let assistant = Message::Assistant(AssistantMessage {
        id: MessageId::new("a"),
        content: vec![ContentBlock::Text {
            text: "x".repeat(400),
        }],
        model: "m".to_owned(),
        stop_reason: None,
        usage: Some(Usage {
            input_tokens: 100,
            output_tokens: 10,
            cache_creation_input_tokens: 5,
            cache_read_input_tokens: 5,
        }),
    });
    let before = user(&"y".repeat(4000));
    let after = user(&"z".repeat(40));
    let active = [&before, &assistant, &after];
    assert_eq!(count(&active), 120 + 10);
    assert_eq!(count(&[&before]), 1000);
}

/// 带内容块的 attachment（`@` 提及的图片，tui-input.md §1.3）：图片按估算值计入。
#[test]
fn attachment_blocks_count_like_message_blocks() {
    let attachment = Message::Attachment(rust_cli_kernel_api::message::AttachmentMessage {
        id: MessageId::new("a"),
        source: "file".to_owned(),
        text: "abcdefgh".to_owned(),
        data: serde_json::json!({
            rust_cli_kernel_api::message::ATTACHMENT_BLOCKS: [
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "x"}},
                {"type": "text", "text": "abcd"},
            ],
        }),
    });
    assert_eq!(message(&attachment), 2 + IMAGE_TOKENS + 1);
}
