//! 任务提醒：轮数怎么数、文字。

use rust_cli_kernel_api::agents::{TaskRecord, TaskStatus};
use rust_cli_kernel_api::ids::{MessageId, ToolUseId};
use rust_cli_kernel_api::message::{AssistantMessage, AttachmentMessage, ContentBlock, Message};
use serde_json::Value;

use super::{SOURCE, due, render};

fn text(text: &str) -> ContentBlock {
    ContentBlock::Text {
        text: text.to_owned(),
    }
}

fn call(name: &str) -> ContentBlock {
    ContentBlock::ToolUse {
        id: ToolUseId::new("t"),
        name: name.to_owned(),
        input: Value::Null,
    }
}

fn thinking() -> ContentBlock {
    ContentBlock::Thinking {
        thinking: "hm".to_owned(),
        signature: None,
    }
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

fn reminder() -> Message {
    Message::Attachment(AttachmentMessage {
        id: MessageId::new("r"),
        source: SOURCE.to_owned(),
        text: String::new(),
        data: Value::Null,
    })
}

fn replies(count: usize) -> Vec<Message> {
    (0..count).map(|_| assistant(vec![text("ok")])).collect()
}

#[test]
fn ten_replies_without_task_tools_are_due() {
    assert!(!due(&replies(9)));
    assert!(due(&replies(10)));
    assert!(!due(&[]));
}

/// 一个回复里的每个块算一轮（thinking 不算）；TaskCreate / TaskUpdate 之后的块才算。
#[test]
fn blocks_after_the_last_task_call_are_counted() {
    let mut history = vec![assistant(vec![call("TaskCreate"), text("a"), text("b")])];
    history.extend(replies(7));
    assert!(!due(&history));
    history.push(assistant(vec![thinking(), text("c")]));
    assert!(due(&history));
    // 别的工具不算用过任务工具。
    let mut history = vec![assistant(vec![call("TaskList")])];
    history.extend(replies(9));
    assert!(due(&history));
}

/// 上次提醒之后还要再过十轮。
#[test]
fn a_reminder_resets_its_own_count() {
    let mut history = replies(12);
    history.push(reminder());
    history.extend(replies(9));
    assert!(!due(&history));
    history.extend(replies(1));
    assert!(due(&history));
}

fn task(id: &str, subject: &str, status: TaskStatus) -> TaskRecord {
    TaskRecord {
        id: id.to_owned(),
        subject: subject.to_owned(),
        description: String::new(),
        active_form: None,
        owner: None,
        status,
        blocks: Vec::new(),
        blocked_by: Vec::new(),
        metadata: None,
    }
}

const TEXT: &str = "The task tools haven't been used recently. If you're working on tasks that would benefit from tracking progress, consider using TaskCreate to add new tasks and TaskUpdate to update task status (set to in_progress when starting, completed when done). Also consider cleaning up the task list if it has become stale. Only use these if relevant to the current work. This is just a gentle reminder - ignore if not applicable.";

#[test]
fn the_text_lists_the_tasks() {
    let (empty, data) = render(&[]);
    assert_eq!(empty, format!("{TEXT}\n"));
    assert_eq!(data["itemCount"], 0);
    let tasks = [
        task("1", "Write tests", TaskStatus::Completed),
        task("2", "Fix parser", TaskStatus::InProgress),
    ];
    let (listed, data) = render(&tasks);
    assert_eq!(
        listed,
        format!(
            "{TEXT}\n\n\nHere are the existing tasks:\n\n#1. [completed] Write tests\n#2. [in_progress] Fix parser"
        )
    );
    assert_eq!(data["itemCount"], 2);
    assert_eq!(data["content"][1]["subject"], "Fix parser");
}
