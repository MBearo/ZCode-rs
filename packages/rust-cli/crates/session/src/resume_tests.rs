use rust_cli_kernel_api::ids::{MessageId, ToolUseId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, UserMessage, UserOrigin,
};
use serde_json::json;

use super::{INTERRUPTED_CALL, settle};

fn assistant(id: &str, calls: &[&str]) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new(id),
        content: calls
            .iter()
            .map(|call| ContentBlock::ToolUse {
                id: ToolUseId::new(*call),
                name: "Bash".into(),
                input: json!({}),
            })
            .collect(),
        model: "m".into(),
        stop_reason: None,
        usage: None,
    })
}

fn result(id: &str, call: &str, owner: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new(call),
            content: Vec::new(),
            is_error: false,
        }],
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new(owner),
        },
    })
}

fn human(id: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: "go".to_owned(),
        }],
        origin: UserOrigin::Human,
    })
}

/// (消息 ID, 块的简写)：调用 `use:<id>`，结果 `res:<id>[:err]`，文字 `t`。
fn shape(messages: &[Message]) -> Vec<(String, Vec<String>)> {
    messages
        .iter()
        .map(|message| {
            let blocks: &[ContentBlock] = match message {
                Message::User(user) => &user.content,
                Message::Assistant(assistant) => &assistant.content,
                _ => &[],
            };
            let blocks = blocks
                .iter()
                .map(|block| match block {
                    ContentBlock::ToolUse { id, .. } => format!("use:{id}"),
                    ContentBlock::ToolResult {
                        tool_use_id,
                        is_error,
                        content,
                    } => {
                        let interrupted = matches!(content.as_slice(),
                            [rust_cli_kernel_api::message::ToolResultContent::Text { text }] if text == INTERRUPTED_CALL);
                        format!(
                            "res:{tool_use_id}{}{}",
                            if *is_error { ":err" } else { "" },
                            if interrupted { ":interrupted" } else { "" }
                        )
                    }
                    _ => "t".to_owned(),
                })
                .collect();
            (message.id().to_string(), blocks)
        })
        .collect()
}

fn s(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn ids() -> impl FnMut() -> MessageId {
    let mut next = 0;
    move || {
        next += 1;
        MessageId::new(format!("added{next}"))
    }
}

/// 末尾回复里没有结果的调用留着，按块的顺序在最后补中断结果；更早的回复里没有结果的调用只去掉它的块，回复空了
/// 才整条去掉。
#[test]
fn trailing_calls_get_results_and_earlier_ones_lose_their_blocks() {
    let messages = vec![
        human("u1"),
        assistant("a1", &["t0"]),
        human("u2"),
        assistant("a2", &["t1", "t2", "t3"]),
        result("r1", "t1", "a2"),
    ];
    let (settled, added) = settle(messages, ids());
    assert_eq!(
        shape(&settled),
        vec![
            ("u1".to_owned(), s(&["t"])),
            ("u2".to_owned(), s(&["t"])),
            ("a2".to_owned(), s(&["use:t1", "use:t2", "use:t3"])),
            ("r1".to_owned(), s(&["res:t1"])),
            ("added1".to_owned(), s(&["res:t2:err:interrupted"])),
            ("added2".to_owned(), s(&["res:t3:err:interrupted"])),
        ]
    );
    assert_eq!(added, [MessageId::new("added1"), MessageId::new("added2")]);
}

/// 都有结果时原样返回，不补。
#[test]
fn a_settled_history_is_left_alone() {
    let messages = vec![
        human("u1"),
        assistant("a1", &["t1"]),
        result("r1", "t1", "a1"),
    ];
    let (settled, added) = settle(messages.clone(), ids());
    assert_eq!(settled, messages);
    assert!(added.is_empty());
}

/// 往前找末尾时越过 skill 正文一类的 meta 消息；越过回复之后遇到的 meta 不再越过。
#[test]
fn meta_messages_after_the_results_do_not_hide_the_trailing_calls() {
    let meta = Message::User(UserMessage {
        id: MessageId::new("m1"),
        content: vec![ContentBlock::Text {
            text: "skill body".to_owned(),
        }],
        origin: UserOrigin::Meta {
            source: "skill".to_owned(),
        },
    });
    let messages = vec![
        human("u1"),
        assistant("a1", &["t1", "t2"]),
        result("r1", "t1", "a1"),
        meta,
    ];
    let (settled, added) = settle(messages, ids());
    assert_eq!(added.len(), 1);
    assert_eq!(
        shape(&settled).last().unwrap(),
        &("added1".to_owned(), s(&["res:t2:err:interrupted"]))
    );
}
