use serde_json::json;

use crate::ids::{MessageId, ToolUseId};
use crate::message::{AssistantMessage, ContentBlock, Message, UserMessage, UserOrigin};

use super::{touched_file, touched_files};

fn call(id: &str, name: &str, path: &str) -> ContentBlock {
    ContentBlock::ToolUse {
        id: ToolUseId::new(id),
        name: name.to_owned(),
        input: json!({"file_path": path}),
    }
}

fn result(id: &str, is_error: bool) -> ContentBlock {
    ContentBlock::ToolResult {
        tool_use_id: ToolUseId::new(id),
        content: Vec::new(),
        is_error,
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

fn user(content: Vec<ContentBlock>) -> Message {
    Message::User(UserMessage {
        id: MessageId::new("u"),
        content,
        origin: UserOrigin::Human,
    })
}

#[test]
fn only_read_write_and_edit_with_a_path_count() {
    assert_eq!(
        touched_file("Read", &json!({"file_path": "/w/a"})),
        Some("/w/a")
    );
    assert_eq!(touched_file("Edit", &json!({"file_path": ""})), None);
    assert_eq!(touched_file("Write", &json!({})), None);
    assert_eq!(
        touched_file("NotebookEdit", &json!({"file_path": "/w/a"})),
        None
    );
    assert_eq!(touched_file("Bash", &json!({"file_path": "/w/a"})), None);
}

/// 成功：结果也在历史里、不是错误。按调用顺序，不去重。
#[test]
fn successful_calls_in_call_order() {
    let history = [
        assistant(vec![
            call("t1", "Read", "/w/a"),
            call("t2", "Edit", "/w/b"),
            call("t3", "Write", "/w/c"),
            call("t4", "Read", "/w/a"),
            call("t5", "Glob", "/w/d"),
        ]),
        user(vec![
            result("t1", false),
            result("t2", true),
            result("t4", false),
            result("t5", false),
        ]),
    ];
    assert_eq!(touched_files(&history), ["/w/a", "/w/a"]);
}
