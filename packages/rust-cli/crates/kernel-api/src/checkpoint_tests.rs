use serde_json::json;

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::ids::{AgentId, ArtifactId, InteractionId, OperationId, RunId, SessionId, TurnId};
use crate::message::{ToolResultContent, UserMessage};
use crate::store::ArtifactPurpose;

/// 只给 operation 用的顺序 ID（testing 模块在本 crate 的单测里不可用）。
#[derive(Default)]
struct SequentialIds(AtomicU64);

impl SequentialIds {
    fn next(&self) -> String {
        format!("id{}", self.0.fetch_add(1, Ordering::SeqCst))
    }
}

impl Ids for SequentialIds {
    fn session(&self) -> SessionId {
        SessionId::new(self.next())
    }
    fn subagent(&self) -> (AgentId, SessionId) {
        (AgentId::new(self.next()), SessionId::new(self.next()))
    }
    fn message(&self) -> MessageId {
        MessageId::new(self.next())
    }
    fn turn(&self) -> TurnId {
        TurnId::new(self.next())
    }
    fn run(&self) -> RunId {
        RunId::new(self.next())
    }
    fn operation(&self) -> OperationId {
        OperationId::new(self.next())
    }
    fn interaction(&self) -> InteractionId {
        InteractionId::new(self.next())
    }
}

fn output(is_error: bool, display: Value) -> ToolOutput {
    ToolOutput {
        content: vec![ToolResultContent::Text {
            text: "x".to_owned(),
        }],
        is_error,
        display,
    }
}

fn artifact() -> Value {
    serde_json::to_value(ArtifactRef {
        session: SessionId::new("s"),
        id: ArtifactId::new("checkpoint-tool-result-1.json"),
        purpose: ArtifactPurpose::Checkpoint,
        media_type: "application/json".to_owned(),
        size: 10,
    })
    .unwrap()
}

fn result(tool_use: &str, is_error: bool) -> Message {
    Message::User(UserMessage {
        id: MessageId::new("r1"),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new(tool_use),
            content: Vec::new(),
            is_error,
        }],
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new("a1"),
        },
    })
}

#[test]
fn a_noted_checkpoint_settles_with_its_tool_result_once() {
    let mut pending = PendingCheckpoints::default();
    let ids = SequentialIds::default();
    let display = json!({"checkpoint": {"artifact": artifact(), "path": "/w/a.txt", "additions": 2, "deletions": 1}});
    pending.note(&ToolUseId::new("t1"), &output(false, display));
    pending.note(&ToolUseId::new("t2"), &output(false, Value::Null));
    assert!(
        pending
            .settle(&result("t2", false), &MessageId::new("u1"), &ids)
            .is_empty()
    );
    let settled = pending.settle(&result("t1", false), &MessageId::new("u1"), &ids);
    assert_eq!(settled.len(), 1);
    let (checkpoint, change) = &settled[0];
    assert_eq!(checkpoint.prompt, MessageId::new("u1"));
    assert_eq!(checkpoint.assistant, MessageId::new("a1"));
    assert_eq!(checkpoint.tool_use, ToolUseId::new("t1"));
    assert!(checkpoint.id.starts_with("checkpoint_"));
    assert_eq!(
        change,
        &FileChange {
            path: "/w/a.txt".to_owned(),
            additions: 2,
            deletions: 1,
            machine: None,
        }
    );
    // 已落库的不再给出。
    assert!(
        pending
            .settle(&result("t1", false), &MessageId::new("u1"), &ids)
            .is_empty()
    );
}

/// 结果被中断文本替换（is_error）时 checkpoint 仍然落库：写入已经发生（session-lifecycle.md §4）。
#[test]
fn a_replaced_error_result_still_settles_the_write() {
    let mut pending = PendingCheckpoints::default();
    let ids = SequentialIds::default();
    let display = json!({"checkpoint": {"artifact": artifact(), "path": "/w/a.txt"}});
    pending.note(&ToolUseId::new("t1"), &output(true, display));
    let settled = pending.settle(&result("t1", true), &MessageId::new("u1"), &ids);
    assert_eq!(settled.len(), 1);
}
