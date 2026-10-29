//! 查看子代理时的状态行（docs/specs/rust-cli/tui-panels.md §1.7、stall-visibility.md §4.3，修复线 F112）：跟着被查看的
//! 子代理（列表快照的条目），主会话的状态行与重试行不画。
use std::time::{Duration, Instant};

use rust_cli_kernel_api::ids::{AgentId, SessionId, TurnId};
use rust_cli_kernel_api::model::ModelNotice;
use rust_cli_kernel_api::panels::{SubagentActivity, SubagentEntry, SubagentStatus};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent};
use rust_cli_kernel_api::testing::selection;

use super::*;
use crate::ui::agents::{AgentList, Viewing};
use crate::ui::palette::{ColorLevel, Palette};
use crate::view::Transcript;

fn transcript() -> Transcript {
    Transcript::new(
        selection(),
        PermissionMode::Default,
        crate::view::test_workspace(),
    )
}

/// 主会话的回合在跑（开始于 1000 ms）。
fn app(lang: Lang) -> App {
    let mut app = App::new(
        Palette::new(ColorLevel::TrueColor, Some((0, 0, 0))),
        lang,
        transcript(),
        Some("/Users/dev".into()),
        "0.1.0".into(),
    );
    app.view.apply(
        SequencedEvent {
            seq: 1,
            revision: 0,
            event: KernelEvent::TurnStarted {
                turn: TurnId::new("turn_1"),
                command: None,
            },
        },
        1_000,
    );
    app
}

fn entry(status: SubagentStatus, activity: Option<SubagentActivity>) -> SubagentEntry {
    SubagentEntry {
        agent: AgentId::new("agent_a"),
        child: SessionId::new("sess_subagent_agent_a"),
        agent_type: "Explore".into(),
        description: "Find callers".into(),
        status,
        background: true,
        waiting: false,
        elapsed_ms: 65_000,
        ended_ago_ms: None,
        version: 0,
        owner: None,
        activity,
        parked: false,
        machine: None,
    }
}

/// 查看 agent_a；快照在 `at` 到达。
fn view(app: &mut App, entry: SubagentEntry, at: Instant) {
    app.agents = AgentList {
        entries: vec![entry],
        at: Some(at),
    };
    app.viewing = Some(Viewing {
        agent: AgentId::new("agent_a"),
        kind: "Explore".into(),
        description: "Find callers".into(),
        version: 0,
        status: SubagentStatus::Running,
        messages: Vec::new(),
        owner: None,
        view: transcript(),
    });
}

fn clock(now: Instant) -> Clock {
    Clock {
        now_ms: 30_000,
        animation: Duration::ZERO,
        since_frame: Duration::ZERO,
        now,
    }
}

fn text(app: &App, now: Instant) -> Vec<String> {
    status(app, 120, &clock(now))
        .iter()
        .map(crate::ui::text::plain)
        .collect()
}

/// 被查看的子代理在跑：spinner、动词与它的用时（快照里的 65 s 加收到快照后过了 2 s）；没有 token 数与思考状态。
/// 修前画的是主会话的状态行（`↓ 0 tokens`，用时按主会话的回合）。
#[test]
fn a_running_subagent_has_its_own_status_line() {
    let mut app = app(Lang::En);
    let at = Instant::now();
    let main = text(&app, at);
    assert!(main[0].contains("tokens"), "{main:?}");
    view(&mut app, entry(SubagentStatus::Running, None), at);
    let lines = text(&app, at + Duration::from_secs(2));
    assert_eq!(lines.len(), 1, "{lines:?}");
    let line = &lines[0];
    assert!(line.starts_with("⠋ "), "{line}");
    assert!(line.ends_with("… (1m 7s)"), "{line}");
    assert!(!line.contains("tokens"), "{line}");
    let verb = line
        .trim_start_matches("⠋ ")
        .split('…')
        .next()
        .unwrap()
        .to_owned();
    assert!(VERBS_EN.contains(&verb.as_str()), "{verb}");
    // 动词按子代理固定：快照刷新、时间过去都不变。
    let later = text(&app, at + Duration::from_secs(9));
    assert!(later[0].starts_with(&format!("⠋ {verb}…")), "{later:?}");
}

/// 子代理不在跑：没有状态行，即使主会话在跑（主会话的状态行在查看时不画）。
#[test]
fn a_finished_subagent_has_no_status_line() {
    let mut app = app(Lang::En);
    let at = Instant::now();
    view(&mut app, entry(SubagentStatus::Completed, None), at);
    assert!(text(&app, at).is_empty());
}

/// 子代理的请求在等重试或停住：整行换成重试行（与主线程同一套文字），倒计时减去收到快照后过的时间；主线程自己的
/// 重试行在查看时不画。
#[test]
fn the_subagents_retry_replaces_the_line() {
    let mut app = app(Lang::En);
    app.view.apply(
        SequencedEvent {
            seq: 2,
            revision: 0,
            event: KernelEvent::ModelNotice {
                notice: ModelNotice::Stalled {
                    retry_in_ms: 280_000,
                },
            },
        },
        1_000,
    );
    let at = Instant::now();
    view(
        &mut app,
        entry(
            SubagentStatus::Running,
            Some(SubagentActivity::Retrying {
                attempt: 1,
                max_retries: 10,
                retry_in_ms: 7_000,
                error: "overloaded".into(),
            }),
        ),
        at,
    );
    let lines = text(&app, at + Duration::from_secs(2));
    assert_eq!(
        lines,
        ["✻ API error · Retrying in 5s · attempt 1/10".to_owned()]
    );
    // 第三次起写具体的错误。
    view(
        &mut app,
        entry(
            SubagentStatus::Running,
            Some(SubagentActivity::Retrying {
                attempt: 3,
                max_retries: 10,
                retry_in_ms: 5_000,
                error: "overloaded".into(),
            }),
        ),
        at,
    );
    assert_eq!(
        text(&app, at),
        ["✻ overloaded · Retrying in 5s · attempt 3/10".to_owned()]
    );
    view(
        &mut app,
        entry(
            SubagentStatus::Running,
            Some(SubagentActivity::Stalled {
                retry_in_ms: 280_000,
            }),
        ),
        at,
    );
    assert_eq!(
        text(&app, at),
        ["✻ Waiting for API response · will retry in 4m 40s · check your network".to_owned()]
    );
}

/// 中文：重试行用中文；状态行的动词用中文的那一组。
#[test]
fn chinese_uses_the_chinese_words() {
    let mut app = app(Lang::Zh);
    let at = Instant::now();
    view(
        &mut app,
        entry(
            SubagentStatus::Running,
            Some(SubagentActivity::Retrying {
                attempt: 2,
                max_retries: 10,
                retry_in_ms: 5_000,
                error: "overloaded".into(),
            }),
        ),
        at,
    );
    assert_eq!(
        text(&app, at),
        ["✻ API 错误 · 5s后重试 · 第 2/10 次".to_owned()]
    );
    view(&mut app, entry(SubagentStatus::Running, None), at);
    let line = text(&app, at).remove(0);
    let verb = line
        .trim_start_matches("⠋ ")
        .split('…')
        .next()
        .unwrap()
        .to_owned();
    assert!(VERBS_ZH.contains(&verb.as_str()), "{line}");
}

/// 主会话空闲、被查看的子代理在跑：按动画节拍重画（spinner 是它的）；它结束后不再。
#[test]
fn a_running_viewed_subagent_keeps_the_spinner_moving() {
    let mut app = App::new(
        Palette::new(ColorLevel::TrueColor, Some((0, 0, 0))),
        Lang::En,
        transcript(),
        Some("/Users/dev".into()),
        "0.1.0".into(),
    );
    assert!(!app.animating());
    let at = Instant::now();
    view(&mut app, entry(SubagentStatus::Running, None), at);
    assert!(app.animating());
    view(&mut app, entry(SubagentStatus::Completed, None), at);
    assert!(!app.animating());
}
