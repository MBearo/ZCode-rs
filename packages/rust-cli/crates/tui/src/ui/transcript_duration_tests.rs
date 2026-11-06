//! transcript 里的回合结束那一行（docs/specs/rust-cli/tui-display.md §2.7，修复线 F118）：` · <pill> still running` 每次画时
//! 取在跑的命令（任务行的同一个函数、同一份快照，排版缓存跟着失效）；等后台子代理的行不接它；相同的等待行在普通视图里只显示第一行。
use std::time::Duration;

use rust_cli_kernel_api::env_shell::BackgroundStatus;
use rust_cli_kernel_api::ids::{MessageId, TurnId};
use rust_cli_kernel_api::manage::{BackgroundEntry, BackgroundKind};
use rust_cli_kernel_api::message::{ContentBlock, Message, UserMessage, UserOrigin};
use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent, TurnOutcome};
use rust_cli_kernel_api::turn_duration::TurnDuration;

use super::{Cache, lines};
use crate::ui::app::App;
use crate::ui::cells::Ctx;
use crate::ui::test_support::app;

/// 按顺序喂给视图：`seq` 是已经喂过的个数（视图丢掉不大于已收到的 seq 的事件），时刻是 seq 秒。
fn feed(app: &mut App, seq: &mut u64, events: Vec<KernelEvent>) {
    for event in events {
        *seq += 1;
        let seq = *seq;
        app.view.apply(
            SequencedEvent {
                seq,
                revision: 0,
                event,
            },
            seq * 1000,
        );
    }
}

/// 一个回合：开始、成功结束、回合结束那一行（等 `waiting` 个后台子代理）。
fn turn(app: &mut App, seq: &mut u64, id: &str, waiting_agents: u32) {
    let turn = TurnId::new(id);
    feed(
        app,
        seq,
        vec![
            KernelEvent::TurnStarted {
                turn: turn.clone(),
                command: None,
            },
            KernelEvent::TurnFinished {
                turn: turn.clone(),
                outcome: TurnOutcome::Success,
                usage: None,
            },
            KernelEvent::TurnDuration(TurnDuration {
                turn,
                duration_ms: 2_000,
                finished_at_ms: 0,
                utc_offset_minutes: 0,
                waiting_agents,
            }),
        ],
    );
}

fn typed(app: &mut App, seq: &mut u64, id: &str) {
    feed(
        app,
        seq,
        vec![KernelEvent::MessageDone {
            message: Message::User(UserMessage {
                id: MessageId::new(id),
                content: vec![ContentBlock::Text { text: id.into() }],
                origin: UserOrigin::Human,
            }),
        }],
    );
}

fn running(id: &str, kind: BackgroundKind) -> BackgroundEntry {
    BackgroundEntry::new(id, kind, id, BackgroundStatus::Running)
}

/// transcript 里 ` ✻ ` 开头的行。
fn shown(app: &App, cache: &mut Cache, expanded: bool) -> Vec<String> {
    let ctx = Ctx {
        ink: &app.ink,
        lang: app.lang,
        width: 80,
        expanded,
        elapsed: Duration::ZERO,
        now_ms: 0,
        workspace: "/Users/dev/project",
        workspace_label: "~/project",
        version: "0.1.0",
        mcp_machines: &[],
        machines: &[],
        skills: &[],
        markdown: None,
    };
    let layout = lines(app, cache, &ctx);
    (0..layout.len())
        .map(|index| layout.styled(index).plain())
        .filter(|line| line.starts_with(" ✻ "))
        .collect()
}

#[test]
fn the_still_running_segment_follows_the_current_pill() {
    let mut app = app();
    let mut seq = 0;
    let mut cache = Cache::default();
    turn(&mut app, &mut seq, "turn_1", 0);
    turn(&mut app, &mut seq, "turn_2", 0);
    assert_eq!(
        shown(&app, &mut cache, false),
        [
            " ✻ Worked for 2s · done 12:00 AM",
            " ✻ Baked for 2s · done 12:00 AM"
        ]
    );
    // 两个回合都在命令运行时结束：每一行都带。
    app.snapshots.background = vec![running("b1", BackgroundKind::Monitor)];
    assert_eq!(
        shown(&app, &mut cache, false),
        [
            " ✻ Worked for 2s · done 12:00 AM · 1 monitor still running",
            " ✻ Baked for 2s · done 12:00 AM · 1 monitor still running"
        ]
    );
    app.snapshots
        .background
        .push(running("b2", BackgroundKind::Shell));
    assert!(
        shown(&app, &mut cache, true)
            .iter()
            .all(|line| line.ends_with("· 1 shell, 1 monitor still running"))
    );
    // 命令结束、快照重取之后：已经显示的行也去掉这一段（同一份缓存）。
    app.snapshots.background.clear();
    assert!(
        shown(&app, &mut cache, false)
            .iter()
            .all(|line| line.ends_with("done 12:00 AM"))
    );
    // 在跑的只有子代理：没有 pill，也没有这一段。
    app.snapshots.background = vec![running("agent_1", BackgroundKind::Agent)];
    assert!(
        shown(&app, &mut cache, false)
            .iter()
            .all(|line| !line.contains("still running"))
    );
}

#[test]
fn the_waiting_line_takes_no_still_running_segment() {
    let mut app = app();
    let mut seq = 0;
    turn(&mut app, &mut seq, "turn_1", 1);
    app.snapshots.background = vec![running("b1", BackgroundKind::Shell)];
    assert_eq!(
        shown(&app, &mut Cache::default(), false),
        [" ✻ Waiting for 1 background agent to finish"]
    );
}

#[test]
fn a_repeated_waiting_line_shows_once_until_the_user_types() {
    let mut app = app();
    let mut seq = 0;
    turn(&mut app, &mut seq, "turn_1", 2);
    turn(&mut app, &mut seq, "turn_2", 2);
    typed(&mut app, &mut seq, "next");
    turn(&mut app, &mut seq, "turn_3", 2);
    turn(&mut app, &mut seq, "turn_4", 1);
    turn(&mut app, &mut seq, "turn_5", 1);
    turn(&mut app, &mut seq, "turn_6", 0);
    let two = " ✻ Waiting for 2 background agents to finish";
    let one = " ✻ Waiting for 1 background agent to finish";
    let done = " ✻ Cooked for 2s · done 12:00 AM";
    assert_eq!(
        shown(&app, &mut Cache::default(), false),
        [two, two, one, done]
    );
    // 详细视图照样显示。
    assert_eq!(
        shown(&app, &mut Cache::default(), true),
        [two, two, two, one, one, done]
    );
}
