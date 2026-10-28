use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::{ContentBlock, Message, UserMessage, UserOrigin};

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

/// 几条消息的估算是每条之和（每条的算法在 kernel-api 的 `message_tokens`，锚点与 attachment 的用例也在那里）。
#[test]
fn messages_sum_each_message() {
    let (a, b) = (user(&"y".repeat(4000)), user(&"z".repeat(40)));
    assert_eq!(messages([&a, &b]), 1000 + 10);
}
