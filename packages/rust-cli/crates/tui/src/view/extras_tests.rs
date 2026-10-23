//! L28 的投影（docs/specs/rust-cli/tui-display.md §2）：任务清单的快照、子代理活动、每回合改动、用量。
use rust_cli_kernel_api::ids::{MessageId, ToolUseId, TurnId};
use rust_cli_kernel_api::message::{AssistantMessage, ContentBlock, Message, Usage};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{FileChange, KernelEvent, SequencedEvent};
use rust_cli_kernel_api::tool::{ToolOutput, ToolProgress};
use serde_json::json;

use super::*;
use crate::view::Body;

fn feed(view: &mut Transcript, seq: u64, event: KernelEvent) {
    view.apply(SequencedEvent { seq, event }, seq * 1000);
}

fn empty() -> Transcript {
    Transcript::new(
        ModelSelection {
            provider: "fixture".into(),
            model: "scripted".into(),
            reasoning: None,
            connection: None,
        },
        PermissionMode::Default,
        crate::view::test_workspace(),
    )
}

fn change(path: &str, additions: u64, deletions: u64) -> FileChange {
    FileChange {
        path: path.into(),
        additions,
        deletions,
    }
}

#[test]
fn tasks_are_replaced_by_each_snapshot() {
    let mut view = empty();
    let task = |id: &str, status: &str| {
        json!({"id": id, "subject": format!("task {id}"), "description": "", "status": status,
               "blocks": [], "blockedBy": []})
    };
    feed(
        &mut view,
        1,
        KernelEvent::TasksChanged {
            tasks: json!([task("1", "pending"), task("2", "in_progress")]),
        },
    );
    assert_eq!(view.tasks().len(), 2);
    assert_eq!(task_counts(view.tasks()), (0, 1, 1));
    feed(
        &mut view,
        2,
        KernelEvent::TasksChanged {
            tasks: json!([task("2", "completed")]),
        },
    );
    assert_eq!(view.tasks().len(), 1);
    assert_eq!(task_counts(view.tasks()), (1, 0, 0));
    // 认不出的快照不清掉上一份。
    feed(&mut view, 3, KernelEvent::TasksChanged { tasks: json!(7) });
    assert_eq!(view.tasks().len(), 1);
}

#[test]
fn file_changes_add_up_per_turn() {
    let mut view = empty();
    let turn = TurnId::new("t1");
    feed(
        &mut view,
        1,
        KernelEvent::FileChanges {
            turn: turn.clone(),
            files: vec![change("a.rs", 2, 1)],
            rewindable: true,
        },
    );
    feed(
        &mut view,
        2,
        KernelEvent::FileChanges {
            turn,
            files: vec![change("a.rs", 3, 0), change("b.rs", 1, 1)],
            rewindable: true,
        },
    );
    feed(
        &mut view,
        3,
        KernelEvent::FileChanges {
            turn: TurnId::new("t2"),
            files: vec![change("c.rs", 4, 0)],
            rewindable: true,
        },
    );
    let turns = view.turn_changes();
    assert_eq!(turns.len(), 2);
    assert_eq!(
        turns[0].files,
        vec![change("a.rs", 5, 1), change("b.rs", 1, 1)]
    );
    assert_eq!(turns[1].files, vec![change("c.rs", 4, 0)]);
}

#[test]
fn usage_adds_up_per_model() {
    let mut view = empty();
    let reply = |id: &str, model: &str, input: u64| {
        Message::Assistant(AssistantMessage {
            id: MessageId::new(id),
            content: vec![ContentBlock::Text { text: "ok".into() }],
            model: model.into(),
            stop_reason: None,
            usage: Some(Usage {
                input_tokens: input,
                output_tokens: 10,
                cache_creation_input_tokens: 1,
                cache_read_input_tokens: 2,
            }),
        })
    };
    feed(
        &mut view,
        1,
        KernelEvent::MessageDone {
            message: reply("m1", "a", 100),
        },
    );
    feed(
        &mut view,
        2,
        KernelEvent::MessageDone {
            message: reply("m2", "a", 50),
        },
    );
    feed(
        &mut view,
        3,
        KernelEvent::MessageDone {
            message: reply("m3", "b", 7),
        },
    );
    let usage = view.usage();
    assert_eq!(usage["a"].input_tokens, 150);
    assert_eq!(usage["a"].output_tokens, 20);
    assert_eq!(usage["a"].cache_read_input_tokens, 4);
    assert_eq!(usage["b"].input_tokens, 7);
    // `/copy N`：1 是最后一条。
    assert_eq!(view.reply_count(), 3);
    assert_eq!(view.nth_reply(1), Some("ok"));
    assert_eq!(view.nth_reply(4), None);
    assert_eq!(view.nth_reply(0), None);
}

#[test]
fn agent_progress_folds_into_the_tool_row() {
    let mut view = empty();
    let agent = ToolUseId::new("agent");
    feed(
        &mut view,
        1,
        KernelEvent::ToolStarted {
            tool_use: agent.clone(),
            name: "Agent".into(),
            input: json!({"description": "look", "prompt": "p"}),
        },
    );
    let progress = |event: serde_json::Value| KernelEvent::ToolProgress {
        tool_use: agent.clone(),
        progress: ToolProgress {
            data: json!({"agentId": "a1", "event": event}),
        },
    };
    feed(
        &mut view,
        2,
        progress(
            json!({"type": "delta", "message": 0, "index": 0, "delta": {"type": "text", "value": "Look"}}),
        ),
    );
    feed(
        &mut view,
        3,
        progress(
            json!({"type": "delta", "message": 0, "index": 0, "delta": {"type": "text", "value": "ing"}}),
        ),
    );
    feed(
        &mut view,
        4,
        progress(
            json!({"type": "tool_started", "tool_use": "r1", "name": "Read",
                        "input": {"file_path": "/Users/dev/project/a.rs"}}),
        ),
    );
    let output = ToolOutput {
        content: Vec::new(),
        is_error: false,
        display: serde_json::Value::Null,
    };
    feed(
        &mut view,
        5,
        progress(json!({"type": "tool_finished", "tool_use": "r1", "output": output})),
    );
    let Body::Tool(tool) = &view.rows().last().unwrap().body else {
        panic!("no tool row");
    };
    let activity = tool.activity.as_ref().expect("activity");
    assert_eq!(activity.tool_uses(), 1);
    assert_eq!(
        activity.items[0],
        Activity::Text {
            message: 0,
            index: 0,
            text: "Looking".into()
        }
    );
    assert!(matches!(
        &activity.items[1],
        Activity::Tool { name, status: ToolStatus::Success, .. } if name == "Read"
    ));
    // Bash 的预览不受影响：没有 agentId 的进度照旧。
    assert!(tool.preview.is_empty());
}

#[test]
fn thinking_deltas_fold_into_their_own_item() {
    // L35（stall-visibility.md §4.2）：推理与文字各自拼成一项。
    let mut view = empty();
    let agent = ToolUseId::new("agent");
    feed(
        &mut view,
        1,
        KernelEvent::ToolStarted {
            tool_use: agent.clone(),
            name: "Agent".into(),
            input: json!({"description": "look", "prompt": "p"}),
        },
    );
    let delta = |index: usize, kind: &str, value: &str| KernelEvent::ToolProgress {
        tool_use: agent.clone(),
        progress: ToolProgress {
            data: json!({"agentId": "a1", "event": {"type": "delta", "message": 1, "index": index,
                         "delta": {"type": kind, "value": value}}}),
        },
    };
    feed(&mut view, 2, delta(0, "thinking", "Let me "));
    feed(&mut view, 3, delta(0, "thinking", "check"));
    feed(&mut view, 4, delta(1, "text", "Found it"));
    let Body::Tool(tool) = &view.rows().last().unwrap().body else {
        panic!("no tool row");
    };
    assert_eq!(
        tool.activity.as_ref().unwrap().items,
        [
            Activity::Thinking {
                message: 1,
                index: 0,
                text: "Let me check".into()
            },
            Activity::Text {
                message: 1,
                index: 1,
                text: "Found it".into()
            }
        ]
    );
}
