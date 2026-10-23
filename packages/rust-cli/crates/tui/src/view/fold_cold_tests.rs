//! 冷加载、补发与会话属性的折叠（docs/specs/rust-cli/tui.md §4.1、§4.2）。
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId, TraceId};
use rust_cli_kernel_api::message::ContentBlock;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent, SessionView};
use rust_cli_kernel_api::store::{SessionKind, SessionMeta};

use super::*;

/// 上下文用量的提示由真实接线的界面测试覆盖（tests/suite/context_low.rs，R5-8：原来这里手工注入
/// ContextUsage，而内核从不发它，测试通过但 footer 永远没有这一项）。
#[test]
fn model_mode_and_title_follow_the_kernel() {
    let mut feed = Feed::new();
    let other = ModelSelection {
        model: "other".into(),
        ..selection()
    };
    feed.push(KernelEvent::ModelChanged {
        selection: other.clone(),
    })
    .push(KernelEvent::ModeChanged {
        mode: PermissionMode::Guarded,
    })
    .push(KernelEvent::PlanChanged { enabled: true })
    .push(KernelEvent::TitleChanged {
        title: "Fix lint".into(),
    });
    assert_eq!(feed.view.selection(), &other);
    // 修复线 F20：权限模式与 plan 开关各自折叠。
    assert_eq!(feed.view.mode(), PermissionMode::Guarded);
    assert!(feed.view.plan());
    feed.push(KernelEvent::PlanChanged { enabled: false });
    assert_eq!(feed.view.mode(), PermissionMode::Guarded);
    assert!(!feed.view.plan());
    // 旧的单值 `ModeChanged{plan}` 按“打开 plan”折叠，权限模式不变。
    feed.push(KernelEvent::ModeChanged {
        mode: PermissionMode::Plan,
    });
    assert_eq!(feed.view.mode(), PermissionMode::Guarded);
    assert!(feed.view.plan());
    assert_eq!(feed.view.title(), "Fix lint");
}

#[test]
fn replayed_events_are_ignored() {
    let mut view = Transcript::new(
        selection(),
        PermissionMode::Default,
        crate::view::test_workspace(),
    );
    let event = SequencedEvent {
        seq: 3,
        event: KernelEvent::MessageDone {
            message: user("u1", "hi"),
        },
    };
    view.apply(event.clone(), 0);
    view.apply(event, 0);
    assert_eq!(view.rows().len(), 1);
    assert_eq!(view.last_seq(), 3);
}

#[test]
fn a_cold_view_rebuilds_rows_with_tool_results() {
    let snapshot = SessionView {
        meta: SessionMeta {
            id: SessionId::new("s1"),
            kind: SessionKind::Interactive,
            parent: None,
            workspace: WorkspaceRef {
                environment: EnvironmentId::new("local"),
                root: EnvPath::new("/Users/dev/project"),
            },
            title: "Old chat".into(),
            trace: TraceId::new("s1"),
            created_at_ms: 0,
        },
        messages: vec![
            user("u1", "list files"),
            assistant(
                "a1",
                vec![
                    ContentBlock::Thinking {
                        thinking: "plan".into(),
                        signature: None,
                    },
                    tool_use("tu1", "Bash"),
                ],
            ),
            tool_result("tu1", "denied", true),
        ],
        selection: selection(),
        mode: PermissionMode::AcceptEdits,
        plan: false,
        pending: Vec::new(),
        queue: Vec::new(),
        last_seq: 7,
        running: false,
        background: Vec::new(),
        peer_held: Vec::new(),
        context_usage: None,
        context_notice: Default::default(),
    };
    let view = Transcript::from_view(snapshot.clone(), 0);
    assert_eq!(view.last_seq(), 7);
    assert_eq!(view.title(), "Old chat");
    assert_eq!(view.mode(), PermissionMode::AcceptEdits);
    let rows = view.rows();
    assert_eq!(rows.len(), 3);
    assert!(matches!(
        rows[1].body,
        Body::Reasoning {
            streaming: false,
            ..
        }
    ));
    let tool = rows[2].tool().unwrap();
    assert_eq!(
        (tool.status, tool.output.as_str()),
        (ToolStatus::Error, "denied")
    );
    assert!(!view.running());
    // 快照时有回合在跑：显示运行中，之后的 TurnFinished 收尾这一行。
    let busy = Transcript::from_view(
        SessionView {
            running: true,
            ..snapshot
        },
        0,
    );
    assert!(busy.running());
}
