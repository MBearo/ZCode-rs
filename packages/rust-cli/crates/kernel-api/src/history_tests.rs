use crate::ids::MessageId;
use crate::message::{
    AssistantMessage, CompactBoundary, CompactTrigger, ContentBlock, Message, PreservedSegment,
    SystemMessage, Usage, UserMessage, UserOrigin,
};

use super::active;

fn user(id: &str, origin: UserOrigin) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: id.to_owned(),
        }],
        origin,
    })
}

fn assistant(id: &str) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: id.to_owned(),
        }],
        model: "m".to_owned(),
        stop_reason: None,
        usage: Some(Usage {
            input_tokens: 10,
            ..Usage::default()
        }),
    })
}

fn boundary(id: &str, preserved: Option<(&str, &str, &str)>) -> Message {
    Message::System(SystemMessage::CompactBoundary(CompactBoundary {
        id: MessageId::new(id),
        trigger: CompactTrigger::Manual,
        pre_tokens: 100,
        summary: MessageId::new("s"),
        preserved: preserved.map(|(head, anchor, tail)| PreservedSegment {
            head: MessageId::new(head),
            anchor: MessageId::new(anchor),
            tail: MessageId::new(tail),
        }),
        discovered_tools: Vec::new(),
    }))
}

fn ids(messages: &[std::borrow::Cow<'_, Message>]) -> Vec<String> {
    messages
        .iter()
        .map(|m| m.id().as_str().to_owned())
        .collect()
}

#[test]
fn without_a_boundary_the_whole_history_is_active() {
    let history = vec![user("u1", UserOrigin::Human), assistant("a1")];
    assert_eq!(ids(&active(&history)), ["u1", "a1"]);
}

#[test]
fn starts_at_the_last_boundary() {
    let history = vec![
        user("u1", UserOrigin::Human),
        assistant("a1"),
        boundary("b1", None),
        user("s", UserOrigin::CompactSummary),
        user("u2", UserOrigin::Human),
        boundary("b2", None),
        user("s2", UserOrigin::CompactSummary),
    ];
    assert_eq!(ids(&active(&history)), ["b2", "s2"]);
}

#[test]
fn relinks_the_preserved_tail_after_the_summary_with_cleared_usage() {
    let history = vec![
        user("u1", UserOrigin::Human),
        assistant("a1"),
        assistant("a2"),
        user(
            "r2",
            UserOrigin::ToolResult {
                assistant: MessageId::new("a2"),
            },
        ),
        boundary("b", Some(("a2", "s", "r2"))),
        user("s", UserOrigin::CompactSummary),
        user(
            "att",
            UserOrigin::Meta {
                source: "file".to_owned(),
            },
        ),
    ];
    let view = active(&history);
    assert_eq!(ids(&view), ["b", "s", "a2", "r2", "att"]);
    match view[2].as_ref() {
        Message::Assistant(assistant) => assert_eq!(assistant.usage, None),
        other => panic!("{other:?}"),
    }
}

#[test]
fn cold_loaded_shape_is_unchanged() {
    let history = vec![
        boundary("b", Some(("a2", "s", "r2"))),
        user("s", UserOrigin::CompactSummary),
        assistant("a2"),
        user(
            "r2",
            UserOrigin::ToolResult {
                assistant: MessageId::new("a2"),
            },
        ),
    ];
    assert_eq!(ids(&active(&history)), ["b", "s", "a2", "r2"]);
}
