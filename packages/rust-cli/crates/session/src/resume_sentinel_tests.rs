//! 续写时补 `No response requested.`（session-lifecycle.md §6.3.1）：哪些结尾补、补在哪。

use rust_cli_kernel_api::ids::{MessageId, ToolUseId};
use rust_cli_kernel_api::message::{
    AssistantMessage, AttachmentMessage, CompactBoundary, CompactTrigger, ContentBlock, Message,
    SystemMessage, ToolResultContent, UserMessage, UserOrigin,
};
use serde_json::Value;

use super::insert;

fn user(id: &str, text: &str, origin: UserOrigin) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        origin,
    })
}

fn human(id: &str, text: &str) -> Message {
    user(id, text, UserOrigin::Human)
}

fn meta(id: &str, text: &str) -> Message {
    user(
        id,
        text,
        UserOrigin::Meta {
            source: "local-command".to_owned(),
        },
    )
}

fn assistant(id: &str) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: "hi".to_owned(),
        }],
        model: "m".to_owned(),
        stop_reason: None,
        usage: None,
    })
}

fn attachment(id: &str) -> Message {
    Message::Attachment(AttachmentMessage {
        id: MessageId::new(id),
        source: "date_change".to_owned(),
        text: String::new(),
        data: Value::Null,
    })
}

fn result(id: &str) -> Message {
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
            assistant: MessageId::new("a"),
        },
    })
}

fn boundary() -> Message {
    Message::System(SystemMessage::CompactBoundary(CompactBoundary {
        id: MessageId::new("b"),
        trigger: CompactTrigger::Manual,
        pre_tokens: 0,
        summary: MessageId::new("s"),
        preserved: None,
        discovered_tools: Vec::new(),
    }))
}

/// 补完之后的消息 ID（补的那条是 `S`）。
fn after(mut messages: Vec<Message>, failed: bool) -> Vec<String> {
    insert(&mut messages, failed, || MessageId::new("S"));
    messages
        .iter()
        .map(|message| {
            if let Message::Assistant(assistant) = message
                && assistant.is_resume_sentinel()
            {
                return "S".to_owned();
            }
            message.id().as_str().to_owned()
        })
        .collect()
}

#[test]
fn an_unanswered_input_gets_the_reply() {
    assert_eq!(after(vec![human("u", "go")], false), ["u", "S"]);
    // 中断提示、工具结果之后同样补。
    let interrupted = vec![
        human("u", "go"),
        human("i", "[Request interrupted by user]"),
    ];
    assert_eq!(after(interrupted, false), ["u", "i", "S"]);
    assert_eq!(
        after(vec![human("u", "go"), assistant("a"), result("r")], false),
        ["u", "a", "r", "S"]
    );
}

#[test]
fn an_answered_history_stays_as_it_is() {
    assert_eq!(
        after(vec![human("u", "go"), assistant("a")], false),
        ["u", "a"]
    );
    assert_eq!(after(Vec::new(), false), Vec::<String>::new());
    // 以 attachment 结尾、前面是回复：不补。
    assert_eq!(
        after(
            vec![human("u", "go"), assistant("a"), attachment("x")],
            false
        ),
        ["u", "a", "x"]
    );
}

/// 当前轮以错误结束（结束时的错误就是最后一条）：不补，新的输入照旧并进去。
#[test]
fn a_failed_turn_gets_no_reply() {
    assert_eq!(
        after(vec![human("u", "go"), attachment("x")], true),
        ["u", "x"]
    );
    assert_eq!(after(vec![human("u", "go")], true), ["u"]);
}

/// 以 attachment 结尾、前面是没有回答的输入或工具结果：补在 attachment 之后；后面的 system 消息留在它后面。
#[test]
fn trailing_attachments_after_an_input_get_the_reply_after_them() {
    assert_eq!(
        after(vec![human("u", "go"), attachment("x")], false),
        ["u", "x", "S"]
    );
    assert_eq!(
        after(
            vec![
                human("u", "go"),
                assistant("a"),
                result("r"),
                attachment("x"),
                boundary()
            ],
            false
        ),
        ["u", "a", "r", "x", "S", "b"]
    );
    // 中断提示与压缩摘要之后的 attachment：不补。
    let interrupted = vec![human("i", "[Request interrupted by user]"), attachment("x")];
    assert_eq!(after(interrupted, false), ["i", "x"]);
    let summary = vec![
        user("s", "summary", UserOrigin::CompactSummary),
        attachment("x"),
    ];
    assert_eq!(after(summary, false), ["s", "x"]);
    // 只有 attachment：补。
    assert_eq!(after(vec![attachment("x")], false), ["x", "S"]);
}

/// 本地命令（caveat、命令、输出）的记录不算等回复。
#[test]
fn a_local_command_record_gets_no_reply() {
    let caveat = || {
        meta(
            "c",
            "<local-command-caveat>Caveat: local command.</local-command-caveat>",
        )
    };
    let record = || human("n", "<command-name>/x</command-name>");
    let output = meta("o", "<local-command-stdout>done</local-command-stdout>");
    assert_eq!(
        after(vec![caveat(), record(), output], false),
        ["c", "n", "o"]
    );
    // 命令行本身是人类输入、前面没有 caveat：要补。
    assert_eq!(after(vec![record()], false), ["n", "S"]);
    // 不是 meta 的 caveat 不算。
    let plain = human("p", "<local-command-caveat>x</local-command-caveat>");
    assert_eq!(after(vec![plain], false), ["p", "S"]);
}
