//! 后台子代理的结果回到主会话（tui-panels.md §1.8；Puffin 的 `• Subagent 描述 finished`）：内核开的回合与工具边界并入的
//! 都推一条通知，标题是 Agent 调用的描述；同一个结果只推一次；冷加载没有。
use rust_cli_kernel_api::ids::{MessageId, ToolUseId};
use rust_cli_kernel_api::message::{
    AttachmentMessage, ContentBlock, Message, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::session_api::KernelEvent;
use serde_json::json;

use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId, TraceId};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::SessionView;
use rust_cli_kernel_api::store::{SessionKind, SessionMeta};

use super::{Feed, assistant, bodies, selection};
use crate::view::{Body, Transcript};

fn note(task: &str, tool_use: Option<&str>, description: &str) -> String {
    let call = tool_use.map_or(String::new(), |id| {
        format!("\n<tool-use-id>{id}</tool-use-id>")
    });
    format!(
        "<system-reminder>\n<task-notification>\n<task-id>{task}</task-id>{call}\n<status>completed</status>\n<summary>Agent \"{description}\" finished</summary>\n</task-notification>\n</system-reminder>"
    )
}

fn meta(id: &str, text: String) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text { text }],
        origin: UserOrigin::Meta {
            source: "task-notification".into(),
        },
    })
}

fn agent_call(id: &str, description: &str) -> Message {
    assistant(
        "a1",
        vec![ContentBlock::ToolUse {
            id: ToolUseId::new(id),
            name: "Agent".into(),
            input: json!({"description": description, "prompt": "go", "run_in_background": true}),
        }],
    )
}

fn snapshot(messages: Vec<Message>) -> SessionView {
    SessionView::new(
        SessionMeta {
            id: SessionId::new("s1"),
            kind: SessionKind::Interactive,
            parent: None,
            workspace: WorkspaceRef {
                environment: EnvironmentId::new("local"),
                root: EnvPath::new("/Users/dev/project"),
            },
            title: String::new(),
            trace: TraceId::new("s1"),
            created_at_ms: 0,
        },
        messages,
        selection(),
        PermissionMode::Default,
    )
}

fn returned(view: &Transcript) -> Vec<String> {
    bodies(view)
        .into_iter()
        .filter_map(|body| match body {
            Body::Returned { title } => Some(title),
            _ => None,
        })
        .collect()
}

#[test]
fn a_returned_subagent_becomes_a_notice_once() {
    let mut feed = Feed::new();
    feed.push(KernelEvent::MessageDone {
        message: agent_call("toolu_a", "Audit dates"),
    })
    // 内核为结果开的回合：标题是 Agent 调用的描述（不是摘要里的）。
    .push(KernelEvent::MessageDone {
        message: meta("m1", note("agent_1", Some("toolu_a"), "ignored")),
    })
    // 同一个结果又到了：不再推。
    .push(KernelEvent::MessageDone {
        message: meta("m2", note("agent_1", Some("toolu_a"), "ignored")),
    })
    // 工具边界并入的；调用不在视图里时取摘要里的描述（转义还原）。
    .push(KernelEvent::MessageDone {
        message: Message::Attachment(AttachmentMessage {
            id: MessageId::new("att1"),
            source: "queued_command".into(),
            text: note("agent_2", None, "Fix &lt;b&gt; &amp; c"),
            data: json!({"prompt": "", "commandMode": "task-notification"}),
        }),
    });
    assert_eq!(returned(&feed.view), ["Audit dates", "Fix <b> & c"]);
    assert!(feed.view.reported("agent_1") && feed.view.reported("agent_2"));
}

#[test]
fn a_cold_load_has_no_notice_but_knows_the_result_arrived() {
    let mut feed = Feed::new();
    feed.push(KernelEvent::MessageDone {
        message: agent_call("toolu_a", "Audit dates"),
    })
    .push(KernelEvent::MessageDone {
        message: meta("m1", note("agent_1", Some("toolu_a"), "Audit dates")),
    });
    assert_eq!(returned(&feed.view).len(), 1);
    let cold = Transcript::from_view(
        snapshot(vec![
            agent_call("toolu_a", "Audit dates"),
            meta("m1", note("agent_1", Some("toolu_a"), "Audit dates")),
        ]),
        0,
    );
    assert!(returned(&cold).is_empty());
    assert!(cold.reported("agent_1"));
}
