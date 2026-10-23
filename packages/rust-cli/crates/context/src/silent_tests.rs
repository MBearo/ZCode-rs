use rust_cli_kernel_api::ids::{MessageId, ToolUseId};
use rust_cli_kernel_api::message::{AttachmentMessage, ToolResultContent, UserMessage};

use super::*;

fn human() -> Message {
    Message::User(UserMessage {
        id: MessageId::new("u"),
        content: vec![ContentBlock::Text {
            text: "go".to_owned(),
        }],
        origin: UserOrigin::Human,
    })
}

fn call(name: &str, text: Option<&str>) -> Vec<Message> {
    let mut content = Vec::new();
    if let Some(text) = text {
        content.push(ContentBlock::Text {
            text: text.to_owned(),
        });
    }
    content.push(ContentBlock::ToolUse {
        id: ToolUseId::new("t"),
        name: name.to_owned(),
        input: Value::Null,
    });
    vec![
        Message::Assistant(AssistantMessage {
            id: MessageId::new("a"),
            content,
            model: "m".to_owned(),
            stop_reason: None,
            usage: None,
        }),
        Message::User(UserMessage {
            id: MessageId::new("r"),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: ToolUseId::new("t"),
                content: vec![ToolResultContent::Text {
                    text: "ok".to_owned(),
                }],
                is_error: false,
            }],
            origin: UserOrigin::ToolResult {
                assistant: MessageId::new("a"),
            },
        }),
    ]
}

fn reminder() -> Message {
    Message::Attachment(AttachmentMessage {
        id: MessageId::new("s"),
        source: SOURCE.to_owned(),
        text: String::new(),
        data: Value::Null,
    })
}

fn silent_calls(count: usize) -> Vec<Message> {
    let mut history = vec![human()];
    for _ in 0..count {
        history.extend(call("Bash", None));
    }
    history
}

/// 第 5 个没有文本的工具调用之后提醒。
#[test]
fn five_silent_tool_rounds_bring_the_reminder() {
    assert!(announce(&silent_calls(4), true).is_none());
    let (text, _) = announce(&silent_calls(5), true).unwrap();
    assert!(text.starts_with("The user hasn't heard from you in a while."));
    // 模型没有开启时不提醒。
    assert!(announce(&silent_calls(5), false).is_none());
}

#[test]
fn visible_output_and_reminders_reset_the_count() {
    // 中间有一轮说了话：只数它之后的。
    let mut history = silent_calls(3);
    history.extend(call("Bash", Some("Found it.")));
    history.extend(silent_calls(4).into_iter().skip(1));
    assert!(announce(&history, true).is_none());
    // AskUserQuestion 算让用户看到了东西。
    let mut history = silent_calls(4);
    history.extend(call("AskUserQuestion", None));
    assert!(announce(&history, true).is_none());
    // 提醒之后重新数 5 轮；同一段静默里最多 3 次。
    let mut history = silent_calls(5);
    history.push(reminder());
    history.extend(silent_calls(4).into_iter().skip(1));
    assert!(announce(&history, true).is_none());
    history.extend(call("Bash", None));
    assert!(announce(&history, true).is_some());
    let mut history = silent_calls(5);
    for _ in 0..3 {
        history.push(reminder());
        history.extend(silent_calls(5).into_iter().skip(1));
    }
    assert!(announce(&history, true).is_none());
}

#[test]
fn only_mid_turn_steps_are_considered() {
    let mut history = silent_calls(6);
    history.push(human());
    assert!(announce(&history, true).is_none());
}
