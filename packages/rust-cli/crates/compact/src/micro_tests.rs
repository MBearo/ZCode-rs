use rust_cli_kernel_api::ids::{MessageId, ToolUseId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, ToolResultContent, UserMessage, UserOrigin,
};
use serde_json::json;

use super::*;

fn round(index: usize, tool: &str, size: usize) -> [Message; 2] {
    let id = ToolUseId::new(format!("t{index}"));
    [
        Message::Assistant(AssistantMessage {
            id: MessageId::new(format!("a{index}")),
            content: vec![ContentBlock::ToolUse {
                id: id.clone(),
                name: tool.to_owned(),
                input: json!({}),
            }],
            model: "m".to_owned(),
            stop_reason: None,
            usage: None,
        }),
        Message::User(UserMessage {
            id: MessageId::new(format!("r{index}")),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: id,
                content: vec![ToolResultContent::Text {
                    text: "x".repeat(size),
                }],
                is_error: false,
            }],
            origin: UserOrigin::ToolResult {
                assistant: MessageId::new(format!("a{index}")),
            },
        }),
    ]
}

#[test]
fn clears_all_but_the_most_recent_compactable_results() {
    let mut history: Vec<Message> = Vec::new();
    for index in 0..8 {
        history.extend(round(index, "Read", 40_000));
    }
    history.extend(round(8, "Custom", 40_000));
    let (out, plan) = apply(&history, KEEP_RECENT).unwrap();
    assert_eq!(
        plan.cleared,
        ["t0", "t1", "t2"].map(ToolUseId::new).to_vec()
    );
    assert_eq!(plan.tokens_saved, 30_000);
    let result = |message: &Message| match message {
        Message::User(user) => match &user.content[0] {
            ContentBlock::ToolResult { content, .. } => content.clone(),
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    };
    assert_eq!(
        result(&out[1]),
        vec![ToolResultContent::Text {
            text: CLEARED.to_owned()
        }]
    );
    // 第 4 个调用之后的结果保留；非可清理工具不动。
    assert_eq!(result(&out[7]), result(&history[7]));
    assert_eq!(result(&out[17]), result(&history[17]));
    // 再做一次：已清理的不再计入，不足阈值。
    assert!(apply(&out, KEEP_RECENT).is_none());
}

#[test]
fn small_savings_do_nothing() {
    let mut history: Vec<Message> = Vec::new();
    for index in 0..7 {
        history.extend(round(index, "Bash", 100));
    }
    assert!(apply(&history, KEEP_RECENT).is_none());
}
