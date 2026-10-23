use rust_cli_kernel_api::ids::{MessageId, ToolUseId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, UserMessage, UserOrigin,
};
use serde_json::json;

use super::filter_unresolved;

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

/// 所有调用都没有结果的 assistant 去掉，部分有结果的保留。
#[test]
fn assistants_whose_calls_all_lack_results_are_dropped() {
    let messages = vec![
        assistant("a1", &["t1", "t2"]),
        result("r1", "t1", "a1"),
        assistant("a2", &["t3"]),
    ];
    let kept: Vec<String> = filter_unresolved(messages)
        .iter()
        .map(|m| m.id().to_string())
        .collect();
    assert_eq!(kept, ["a1", "r1"]);
}
