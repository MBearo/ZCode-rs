//! 明细（context-usage.md §3.1）：最大余数法、内容块的分类、工具名与调用个数。
use rust_cli_kernel_api::ids::{MessageId, ToolUseId};
use rust_cli_kernel_api::message::{
    AssistantMessage, AttachmentMessage, ToolResultContent, UserMessage,
};
use serde_json::json;

use super::*;

#[test]
fn largest_remainder_adds_up_and_breaks_ties_in_order() {
    assert_eq!(apportion(10, &[1, 1, 1]), [4, 3, 3]);
    assert_eq!(apportion(100, &[1, 2, 3, 4]), [10, 20, 30, 40]);
    // 余数 0.5、0.5、0：先到先得。
    assert_eq!(apportion(5, &[1, 1, 0]), [3, 2, 0]);
    // 2/3 = 0.67 的余数大于 1/3。
    assert_eq!(apportion(1, &[1, 2]), [0, 1]);
    // 权重全是 0：各项按 1。
    assert_eq!(apportion(4, &[0, 0, 0]), [2, 1, 1]);
    assert_eq!(apportion(0, &[5, 7]), [0, 0]);
    assert!(apportion(9, &[]).is_empty());
    let shares = apportion(4729, &[1200, 600, 600, 340, 13]);
    assert_eq!(shares.iter().sum::<u64>(), 4729);
}

fn text(value: &str) -> ContentBlock {
    ContentBlock::Text {
        text: value.to_owned(),
    }
}

fn user(origin: UserOrigin, content: Vec<ContentBlock>) -> Message {
    Message::User(UserMessage {
        id: MessageId::new("u"),
        content,
        origin,
    })
}

fn assistant(content: Vec<ContentBlock>) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new("a"),
        content,
        model: "m".to_owned(),
        stop_reason: None,
        usage: None,
    })
}

fn attachment(source: &str, text: &str, data: Value) -> Message {
    Message::Attachment(AttachmentMessage {
        id: MessageId::new("at"),
        source: source.to_owned(),
        text: text.to_owned(),
        data,
    })
}

fn call(id: &str, name: &str) -> ContentBlock {
    ContentBlock::ToolUse {
        id: ToolUseId::new(id),
        name: name.to_owned(),
        input: json!({}),
    }
}

fn result(id: &str, output: &str) -> ContentBlock {
    ContentBlock::ToolResult {
        tool_use_id: ToolUseId::new(id),
        content: vec![ToolResultContent::Text {
            text: output.to_owned(),
        }],
        is_error: false,
    }
}

fn history() -> Vec<Message> {
    let tool_results = || UserOrigin::ToolResult {
        assistant: MessageId::new("a"),
    };
    vec![
        user(UserOrigin::CompactSummary, vec![text("summary of before")]),
        user(UserOrigin::Human, vec![text("fix the bug")]),
        attachment(
            "todo_reminder",
            "<system-reminder>todo</system-reminder>",
            Value::Null,
        ),
        // 只供显示的 attachment：不发给模型，不算。
        attachment("max_turns_reached", "", Value::Null),
        assistant(vec![
            ContentBlock::Thinking {
                thinking: "hmm".to_owned(),
                signature: None,
            },
            text("Looking."),
            call("t1", "Read"),
            call("t2", "Bash"),
            call("t3", "Read"),
        ]),
        user(
            tool_results(),
            vec![
                result("t1", "file"),
                result("t2", "ok"),
                result("t3", "more"),
            ],
        ),
        // 压缩边界之前的调用：找不到名字。
        user(tool_results(), vec![result("gone", "old")]),
        attachment(
            queued_command::SOURCE,
            "<system-reminder>also this</system-reminder>",
            json!({ queued_command::MODE_KEY: queued_command::PROMPT_MODE }),
        ),
        user(
            UserOrigin::Meta {
                source: "hook".to_owned(),
            },
            vec![text("blocked")],
        ),
        assistant(vec![
            ContentBlock::ServerToolUse {
                id: "srv1".to_owned(),
                name: "web_search".to_owned(),
                input: json!({}),
            },
            ContentBlock::WebSearchToolResult {
                tool_use_id: "srv1".to_owned(),
                content: json!([]),
            },
        ]),
    ]
}

#[test]
fn blocks_are_classified_by_where_they_come_from() {
    let history = history();
    let messages: Vec<&Message> = history.iter().collect();
    let found = pieces(&messages, 4);
    let kinds: Vec<(Kind, Option<&str>)> = found
        .iter()
        .map(|piece| (piece.kind, piece.tool.as_deref()))
        .collect();
    assert_eq!(
        kinds,
        [
            (Kind::Summary, None),
            (Kind::Yours, None),
            (Kind::Attachments, None),
            (Kind::Thinking, None),
            (Kind::Replies, None),
            (Kind::Calls, None),
            (Kind::Calls, None),
            (Kind::Calls, None),
            (Kind::Results, Some("Read")),
            (Kind::Results, Some("Bash")),
            (Kind::Results, Some("Read")),
            (Kind::Results, Some("Unknown")),
            (Kind::Yours, None),
            (Kind::Attachments, None),
            (Kind::Calls, None),
            (Kind::Results, Some("web_search")),
        ]
    );
    // 权重与消息估算的每块相同。
    assert_eq!(found[1].weight, counts::rough("fix the bug", 4));
    assert_eq!(found[8].weight, counts::block(&result("t1", "file"), 4));
}

#[test]
fn kinds_and_tool_results_add_up_with_their_call_counts() {
    let history = history();
    let messages: Vec<&Message> = history.iter().collect();
    let found = pieces(&messages, 4);
    let (kinds, tools) = super::messages(1000, &found);
    assert_eq!(kinds.iter().map(|kind| kind.tokens).sum::<u64>(), 1000);
    assert_eq!(
        kinds
            .iter()
            .map(|kind| kind.kind.as_str())
            .collect::<Vec<_>>(),
        [
            "Your messages",
            "Replies",
            "Thinking",
            "Tool calls",
            "Tool results",
            "Attachments",
            "Summary",
        ]
    );
    let results = kinds
        .iter()
        .find(|kind| kind.kind == "Tool results")
        .unwrap()
        .tokens;
    assert_eq!(tools.iter().map(|tool| tool.tokens).sum::<u64>(), results);
    let calls: Vec<(&str, u64)> = tools
        .iter()
        .map(|tool| (tool.tool.as_str(), tool.calls))
        .collect();
    assert_eq!(
        calls,
        [("Read", 2), ("Bash", 1), ("Unknown", 1), ("web_search", 1)]
    );

    // 类别是 0：没有明细。
    let (kinds, tools) = super::messages(0, &found);
    assert!(kinds.is_empty() && tools.is_empty());
}
