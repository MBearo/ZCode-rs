use rust_cli_kernel_api::ids::{MessageId, ToolUseId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, ToolResultContent, UserMessage, UserOrigin,
};

use super::*;

fn human(id: &str, text: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        origin: UserOrigin::Human,
    })
}

fn assistant(id: &str) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: "x".repeat(400),
        }],
        model: "m".to_owned(),
        stop_reason: None,
        usage: None,
    })
}

fn result(id: &str, assistant: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new("t"),
            content: vec![ToolResultContent::Text {
                text: "ok".to_owned(),
            }],
            is_error: false,
        }],
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new(assistant),
        },
    })
}

fn ids(messages: &[&Message]) -> Vec<String> {
    messages
        .iter()
        .map(|m| m.id().as_str().to_owned())
        .collect()
}

#[test]
fn groups_start_at_each_assistant() {
    let history = [
        human("u1", "hi"),
        assistant("a1"),
        result("r1", "a1"),
        assistant("a2"),
        human("u2", "more"),
        assistant("a3"),
    ];
    let refs: Vec<&Message> = history.iter().collect();
    let groups = group(&refs);
    let shapes: Vec<Vec<String>> = groups.iter().map(|g| ids(g)).collect();
    assert_eq!(
        shapes,
        [vec!["u1"], vec!["a1", "r1"], vec!["a2", "u2"], vec!["a3"]]
    );
}

#[test]
fn split_all_keeps_the_human_input_after_the_last_reply() {
    let history = [
        human("u1", "hi"),
        assistant("a1"),
        result("r1", "a1"),
        human("u2", "next"),
    ];
    let refs: Vec<&Message> = history.iter().collect();
    let (summarize, keep) = split_all(&refs);
    assert_eq!(ids(&summarize), ["u1", "a1", "r1"]);
    assert_eq!(ids(&keep), ["u2"]);
    let short = [human("u1", "hi"), assistant("a1")];
    let refs: Vec<&Message> = short.iter().collect();
    let (summarize, keep) = split_all(&refs);
    assert_eq!(ids(&summarize), ["u1", "a1"]);
    assert!(keep.is_empty());
}

#[test]
fn substantive_needs_an_assistant_or_long_human_input() {
    let short = human("u", "hi");
    assert!(!substantive(&[&short]));
    let long = human("u", &"y".repeat(4_000));
    assert!(substantive(&[&long]));
    let reply = assistant("a");
    assert!(substantive(&[&short, &reply]));
}

#[test]
fn preserve_step_follows_the_token_gap() {
    // 5 组，摘要前 4 组；每组 100：差 150 要多留 2 组。
    let tokens = [100, 100, 100, 100, 100];
    assert_eq!(preserve_step(&tokens, 4, Some(150)), 2);
    assert_eq!(preserve_step(&tokens, 4, None), 1);
    // 差太大，几乎全要留：取一半。
    assert_eq!(preserve_step(&tokens, 4, Some(10_000)), 2);
}

#[test]
fn truncate_head_drops_whole_groups_and_flags_a_leading_assistant() {
    let history = [
        human("u1", &"y".repeat(400)),
        assistant("a1"),
        assistant("a2"),
        assistant("a3"),
    ];
    let refs: Vec<&Message> = history.iter().collect();
    let (kept, marker) = truncate_head(&refs, Some(50)).unwrap();
    assert_eq!(ids(&kept), ["a1", "a2", "a3"]);
    assert!(marker);
    let (kept, _) = truncate_head(&refs, Some(150)).unwrap();
    assert_eq!(ids(&kept), ["a2", "a3"]);
    // 至少留一组
    let (kept, _) = truncate_head(&refs, Some(1_000_000)).unwrap();
    assert_eq!(ids(&kept), ["a3"]);
    assert!(truncate_head(&refs[..1], None).is_none());
}
