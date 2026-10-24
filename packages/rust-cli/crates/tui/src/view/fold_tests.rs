//! 事件 → 行的折叠（docs/specs/rust-cli/tui.md §4.2）。
use rust_cli_kernel_api::ids::{CommandId, InteractionId, MessageId, ToolUseId, TurnId};
use rust_cli_kernel_api::message::{
    AssistantMessage, AttachmentMessage, ContentBlock, Message, ToolResultContent, UserMessage,
    UserOrigin,
};
use rust_cli_kernel_api::model::{BlockDelta, ModelSelection};
use rust_cli_kernel_api::orchestration::InteractionRequest;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    Delivery, KernelEvent, QueuedInput, SequencedEvent, TurnOutcome,
};
use rust_cli_kernel_api::tool::{ToolOutput, ToolProgress};
use serde_json::json;

use super::*;

fn selection() -> ModelSelection {
    ModelSelection {
        provider: "fixture".into(),
        model: "scripted".into(),
        reasoning: None,
        connection: None,
    }
}

/// 按顺序给事件编 seq 并折叠。
struct Feed {
    view: Transcript,
    seq: u64,
}

impl Feed {
    fn new() -> Self {
        Self {
            view: Transcript::new(
                selection(),
                PermissionMode::Default,
                crate::view::test_workspace(),
            ),
            seq: 0,
        }
    }

    fn push(&mut self, event: KernelEvent) -> &mut Self {
        self.seq += 1;
        self.view.apply(
            SequencedEvent {
                seq: self.seq,
                event,
            },
            self.seq * 1000,
        );
        self
    }
}

fn user(id: &str, text: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text { text: text.into() }],
        origin: UserOrigin::Human,
    })
}

fn assistant(id: &str, content: Vec<ContentBlock>) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new(id),
        content,
        model: "scripted".into(),
        stop_reason: None,
        usage: None,
    })
}

fn tool_use(id: &str, name: &str) -> ContentBlock {
    ContentBlock::ToolUse {
        id: ToolUseId::new(id),
        name: name.into(),
        input: json!({"command": "ls"}),
    }
}

fn tool_result(id: &str, text: &str, is_error: bool) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(format!("r_{id}")),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new(id),
            content: vec![ToolResultContent::Text { text: text.into() }],
            is_error,
        }],
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new("a1"),
        },
    })
}

fn delta(response: u32, index: usize, text: &str) -> KernelEvent {
    KernelEvent::StreamDelta {
        turn: TurnId::new("t1"),
        response,
        index,
        delta: BlockDelta::Text(text.into()),
    }
}

fn started() -> KernelEvent {
    KernelEvent::TurnStarted {
        turn: TurnId::new("t1"),
        command: Some(CommandId::new("c1")),
    }
}

fn finished(outcome: TurnOutcome) -> KernelEvent {
    KernelEvent::TurnFinished {
        turn: TurnId::new("t1"),
        outcome,
        usage: None,
    }
}

fn bodies(view: &Transcript) -> Vec<Body> {
    view.rows().iter().map(|row| row.body.clone()).collect()
}

#[test]
fn a_streamed_reply_becomes_the_final_message() {
    let mut feed = Feed::new();
    feed.push(started())
        .push(KernelEvent::MessageDone {
            message: user("u1", "hi"),
        })
        .push(delta(0, 0, "hel"))
        .push(delta(0, 0, "lo"));
    assert!(feed.view.running());
    assert_eq!(
        bodies(&feed.view)[2],
        Body::Agent {
            text: "hello".into(),
            streaming: true
        }
    );
    assert_eq!(feed.view.streamed_chars(), 5);
    feed.push(KernelEvent::MessageDone {
        message: assistant(
            "a1",
            vec![ContentBlock::Text {
                text: "hello".into(),
            }],
        ),
    })
    .push(finished(TurnOutcome::Success));
    assert_eq!(
        bodies(&feed.view)[1..],
        [
            Body::User { text: "hi".into() },
            Body::Agent {
                text: "hello".into(),
                streaming: false
            }
        ]
    );
    assert!(!feed.view.running());
    assert_eq!(feed.view.last_reply(), Some("hello"));
}

#[test]
fn a_tool_started_while_streaming_keeps_one_row_in_block_order() {
    let mut feed = Feed::new();
    feed.push(started())
        .push(delta(0, 0, "Let me look."))
        .push(KernelEvent::ToolStarted {
            tool_use: ToolUseId::new("tu1"),
            name: "Bash".into(),
            input: json!({"command": "ls"}),
        })
        .push(KernelEvent::ToolProgress {
            tool_use: ToolUseId::new("tu1"),
            progress: ToolProgress {
                data: json!({"type": "bash_progress", "output": "a\nb\n"}),
            },
        })
        .push(KernelEvent::MessageDone {
            message: assistant(
                "a1",
                vec![
                    ContentBlock::Text {
                        text: "Let me look.".into(),
                    },
                    tool_use("tu1", "Bash"),
                ],
            ),
        });
    let rows = feed.view.rows();
    assert_eq!(rows.len(), 3);
    let tool = rows[2].tool().unwrap();
    assert_eq!(tool.status, ToolStatus::Running);
    assert_eq!(tool.preview, "a\nb");
    feed.push(KernelEvent::ToolDone {
        tool_use: ToolUseId::new("tu1"),
        output: ToolOutput {
            content: vec![ToolResultContent::Text {
                text: "a\nb".into(),
            }],
            is_error: false,
            display: serde_json::Value::Null,
        },
    })
    .push(KernelEvent::MessageDone {
        message: tool_result("tu1", "ignored: already has output", false),
    });
    let tool = feed.view.rows()[2].tool().unwrap();
    assert_eq!(
        (tool.status, tool.output.as_str()),
        (ToolStatus::Success, "a\nb")
    );
}

#[test]
fn an_interrupt_settles_streaming_rows_and_running_tools() {
    let mut feed = Feed::new();
    feed.push(started())
        .push(delta(0, 0, "partial"))
        .push(KernelEvent::ToolStarted {
            tool_use: ToolUseId::new("tu1"),
            name: "Bash".into(),
            input: json!({}),
        })
        .push(finished(TurnOutcome::Interrupted));
    let rows = feed.view.rows();
    assert_eq!(rows[0].body, Body::Turn(TurnState::Interrupted));
    assert_eq!(
        rows[1].body,
        Body::Agent {
            text: "partial".into(),
            streaming: false
        }
    );
    assert_eq!(rows[2].tool().unwrap().status, ToolStatus::Cancelled);
    assert!(rows.iter().all(|row| !row.live()));
}

#[test]
fn a_failed_turn_shows_its_reason() {
    let mut feed = Feed::new();
    feed.push(started()).push(finished(TurnOutcome::Failed {
        message: "busy".into(),
    }));
    assert_eq!(
        bodies(&feed.view),
        [Body::Turn(TurnState::Failed), Body::Error("busy".into())]
    );
}

#[test]
fn permission_asks_mark_the_tool_and_resolve() {
    let mut feed = Feed::new();
    let ask = InteractionRequest::Permission {
        machine: None,
        tool_use: ToolUseId::new("tu1"),
        tool: "Bash".into(),
        input: json!({"command": "rm x"}),
        message: "rm needs approval".into(),
        suggestions: Vec::new(),
        asked_by: None,
    };
    feed.push(started())
        .push(KernelEvent::ToolStarted {
            tool_use: ToolUseId::new("tu1"),
            name: "Bash".into(),
            input: json!({"command": "rm x"}),
        })
        .push(KernelEvent::InteractionAsked {
            interaction: InteractionId::new("i1"),
            request: ask.clone(),
        });
    assert_eq!(feed.view.pending(), Some(&(InteractionId::new("i1"), ask)));
    assert_eq!(
        feed.view.rows()[1].tool().unwrap().status,
        ToolStatus::Approval
    );
    feed.push(KernelEvent::InteractionResolved {
        interaction: InteractionId::new("i1"),
    });
    assert!(feed.view.pending().is_none());
    assert_eq!(
        feed.view.rows()[1].tool().unwrap().status,
        ToolStatus::Running
    );
}

#[test]
fn steered_input_leaves_the_queue_and_shows_as_a_user_row() {
    let mut feed = Feed::new();
    let queued = QueuedInput {
        command: CommandId::new("c2"),
        text: "also this".into(),
        delivery: Delivery::Steer,
    };
    feed.push(started()).push(KernelEvent::QueueChanged {
        items: vec![queued.clone()],
    });
    assert_eq!(feed.view.queue(), [queued]);
    feed.push(KernelEvent::QueueChanged { items: Vec::new() })
        .push(KernelEvent::MessageDone {
            message: Message::Attachment(AttachmentMessage {
                id: MessageId::new("att1"),
                source: "queued_command".into(),
                text: "<system-reminder>…</system-reminder>".into(),
                data: json!({"prompt": "also this", "commandMode": "prompt"}),
            }),
        })
        // L18：主线程在工具边界并入的后台通知也是 queued_command，不显示成用户行。
        .push(KernelEvent::MessageDone {
            message: Message::Attachment(AttachmentMessage {
                id: MessageId::new("att3"),
                source: "queued_command".into(),
                text: "<system-reminder>…</system-reminder>".into(),
                data: json!({"prompt": "<system-reminder>…", "commandMode": "task-notification"}),
            }),
        })
        .push(KernelEvent::MessageDone {
            message: Message::Attachment(AttachmentMessage {
                id: MessageId::new("att2"),
                source: "date_change".into(),
                text: "x".into(),
                data: serde_json::Value::Null,
            }),
        });
    assert!(feed.view.queue().is_empty());
    assert_eq!(
        bodies(&feed.view)[1..],
        [Body::User {
            text: "also this".into()
        }]
    );
}

/// L18：只供显示的系统提示追加一行（不发给模型、冷加载没有）。
#[test]
fn system_notices_become_notice_rows() {
    let mut feed = Feed::new();
    feed.push(started()).push(KernelEvent::SystemNotice {
        level: rust_cli_kernel_api::session_api::NoticeLevel::Warning,
        text: "blocked".into(),
    });
    assert_eq!(
        bodies(&feed.view)[1..],
        [Body::Notice {
            level: rust_cli_kernel_api::session_api::NoticeLevel::Warning,
            text: "blocked".into(),
        }]
    );
}

#[test]
fn versions_change_with_content_and_ids_stay() {
    let mut feed = Feed::new();
    feed.push(delta(0, 0, "a"));
    let first = feed.view.rows()[0].clone();
    feed.push(delta(0, 0, "b"));
    let second = &feed.view.rows()[0];
    assert_eq!(first.id, second.id);
    assert!(second.version > first.version);
}

#[path = "fold_cold_tests.rs"]
mod cold;

/// 工作区只从事件取（R1-3）：EnterWorktree / ExitWorktree / `/cd` 之后的 `WorkspaceChanged` 换掉视图的工作区。
#[test]
fn workspace_changes_replace_the_view_workspace() {
    let mut feed = Feed::new();
    assert_eq!(feed.view.workspace().root.as_str(), "/Users/dev/project");
    let mut moved = crate::view::test_workspace();
    moved.root = rust_cli_kernel_api::env::EnvPath::new("/Users/dev/project/.zcode/worktrees/a");
    feed.push(KernelEvent::WorkspaceChanged {
        workspace: moved.clone(),
    });
    assert_eq!(feed.view.workspace(), &moved);
}

#[cfg(test)]
#[path = "fold_view_tests.rs"]
mod refresh;
