//! fork 子代理的前缀与定义（修复线 L-A，agents.md §8.1.2）。
use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::message::{ContentBlock, Message, ToolResultContent, UserOrigin};
use rust_cli_kernel_api::testing::SequentialIds;
use serde_json::json;

use super::*;

#[test]
fn the_prefix_is_the_history_the_call_and_the_started_result() {
    let history = vec![crate::test_support::report_message("earlier")];
    let input = json!({"description": "d", "prompt": "p", "subagent_type": "fork"});
    let tool_use = ToolUseId::new("toolu_1");
    let prefix = prefix(
        &SequentialIds::default(),
        history.clone(),
        &tool_use,
        input.clone(),
        "model-x",
    );
    assert_eq!(prefix.len(), 3);
    assert_eq!(prefix[0], history[0]);
    let Message::Assistant(assistant) = &prefix[1] else {
        panic!("{:?}", prefix[1]);
    };
    assert_eq!(
        assistant.content,
        [ContentBlock::ToolUse {
            id: tool_use.clone(),
            name: "Agent".into(),
            input,
        }]
    );
    let Message::User(result) = &prefix[2] else {
        panic!("{:?}", prefix[2]);
    };
    assert_eq!(
        result.origin,
        UserOrigin::ToolResult {
            assistant: assistant.id.clone()
        }
    );
    assert_eq!(
        result.content,
        [ContentBlock::ToolResult {
            tool_use_id: tool_use,
            content: vec![ToolResultContent::Text {
                text: "Fork started — processing in background".into()
            }],
            is_error: false,
        }]
    );
    let fork = definition();
    assert_eq!(fork.max_turns, Some(200));
    assert!(fork.background);
    assert_eq!(fork.model.as_deref(), Some("inherit"));
}

#[test]
fn the_directive_follows_the_boilerplate() {
    let text = rust_cli_prompts::agents::fork_directive("Read a.txt.");
    assert!(
        text.starts_with("<fork-boilerplate>\nYou are a worker fork."),
        "{text}"
    );
    assert!(
        text.ends_with("</fork-boilerplate>\n\nYour directive: Read a.txt."),
        "{text}"
    );
}
