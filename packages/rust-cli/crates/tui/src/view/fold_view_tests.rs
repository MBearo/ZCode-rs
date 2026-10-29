//! 只读查看的追加刷新（tui-panels.md §1.4，修复线 F6，审查 R5-11）：在任意位置切开，先建前半再追加后半，行的内容与
//! 运行状态都与对整份快照重建相同。
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId, TraceId};
use rust_cli_kernel_api::message::ContentBlock;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::SessionView;
use rust_cli_kernel_api::store::{SessionKind, SessionMeta};

use super::*;

fn snapshot(messages: Vec<Message>, running: bool) -> SessionView {
    SessionView {
        meta: SessionMeta {
            id: SessionId::new("sess_subagent_agent_1"),
            kind: SessionKind::Subagent,
            parent: Some(SessionId::new("s1")),
            workspace: WorkspaceRef {
                environment: EnvironmentId::new("local"),
                root: EnvPath::new("/Users/dev/project"),
            },
            title: "Look around".into(),
            trace: TraceId::new("s1"),
            created_at_ms: 0,
        },
        messages,
        selection: selection(),
        mode: PermissionMode::Default,
        plan: false,
        pending: Vec::new(),
        queue: Vec::new(),
        last_seq: 0,
        revision: 0,
        running,
        background: Vec::new(),
        peer_held: Vec::new(),
        context_usage: None,
        context_notice: Default::default(),
        output_speed: None,
    }
}

fn history() -> Vec<Message> {
    vec![
        user("u1", "look around"),
        assistant(
            "a1",
            vec![
                ContentBlock::Thinking {
                    thinking: "plan".into(),
                    signature: None,
                },
                ContentBlock::Text {
                    text: "Checking".into(),
                },
                tool_use("tu1", "Bash"),
                tool_use("tu2", "Read"),
            ],
        ),
        tool_result("tu1", "a.txt", false),
        tool_result("tu2", "denied", true),
        assistant(
            "a2",
            vec![ContentBlock::Text {
                text: "Found **one** file.".into(),
            }],
        ),
        user("u2", "again"),
        assistant("a3", vec![tool_use("tu3", "Bash")]),
    ]
}

fn bodies(view: &Transcript) -> Vec<Body> {
    view.rows().iter().map(|row| row.body.clone()).collect()
}

#[test]
fn appending_a_refreshed_snapshot_matches_a_rebuild() {
    let all = history();
    for split in 0..=all.len() {
        for (before, after) in [(true, true), (true, false), (false, false)] {
            let mut view = Transcript::from_view(snapshot(all[..split].to_vec(), before), 1);
            let full = snapshot(all.clone(), after);
            assert!(view.append_view(&full, split, 2));
            let rebuilt = Transcript::from_view(full, 2);
            assert_eq!(
                bodies(&view),
                bodies(&rebuilt),
                "split {split} {before} {after}"
            );
            assert_eq!(view.running(), rebuilt.running());
            assert_eq!(view.usage(), rebuilt.usage());
        }
    }
}

#[test]
fn a_changed_view_field_needs_a_rebuild() {
    let all = history();
    let mut view = Transcript::from_view(snapshot(all[..2].to_vec(), true), 1);
    let mut renamed = snapshot(all, true);
    renamed.meta.title = "Other".into();
    assert!(!view.append_view(&renamed, 2, 2));
}

/// 修复线 F112（tui-panels.md §1.7）：子代理的快照带它的 ctx、上下文提示与最近一个请求的速度；追加刷新时这三项换成新快照里
/// 的值（它们随每次提交变，不算“视图字段变了”，不整体重建）。修前追加只折消息，三项停在第一份快照的值。
#[test]
fn appending_takes_the_new_context_and_speed() {
    use rust_cli_kernel_api::compaction::{ContextNotice, ContextOccupancy};
    use rust_cli_kernel_api::metering::OutputSpeed;
    let all = history();
    let mut view = Transcript::from_view(snapshot(all[..2].to_vec(), true), 1);
    assert_eq!(view.context_usage(), None);
    assert_eq!(view.output_speed(), None);
    let mut next = snapshot(all, true);
    next.context_usage = Some(ContextOccupancy {
        used_tokens: 4_500,
        max_tokens: 9_000,
    });
    next.context_notice = ContextNotice::UntilAutoCompact { percent: 12 };
    let speed = OutputSpeed {
        output_tokens: Some(90),
        duration_ms: 4_000,
        first_content_ms: Some(1_000),
    };
    next.output_speed = Some(speed);
    assert!(view.append_view(&next, 2, 2));
    assert_eq!(view.context_usage().map(|usage| usage.percent()), Some(50));
    assert_eq!(
        view.context_notice(),
        ContextNotice::UntilAutoCompact { percent: 12 }
    );
    assert_eq!(view.output_speed(), Some(speed));
    let rebuilt = Transcript::from_view(next, 2);
    assert_eq!(view.context_usage(), rebuilt.context_usage());
    assert_eq!(view.output_speed(), rebuilt.output_speed());
}
