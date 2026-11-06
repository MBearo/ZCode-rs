//! `/stats` 与 `/goal` 的卡（docs/specs/rust-cli/tui-kit.md §4.2）：速度一行的回合历史、结束了的子代理与合计、目标在跑时
//! 每帧重画（Puffin `stats_more`、`usage.speed.turns`、`goal_moving`）。
use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::message::ToolResultContent;
use rust_cli_kernel_api::metering::{LastRequest, RequestStats};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent};
use rust_cli_kernel_api::testing::selection;
use rust_cli_kernel_api::tool::ToolOutput;
use rust_cli_tui_kit::app::widgets::goal::{GoalState, SessionGoal};
use rust_cli_tui_kit::render::StyledLine;
use serde_json::json;

use super::*;
use crate::ui::i18n::Lang;
use crate::ui::probe::{ColorLevel, Colors};
use crate::view::Transcript;

fn app() -> App {
    App::new(
        Colors::new(ColorLevel::TrueColor, Some((0, 0, 0))),
        Lang::En,
        Transcript::new(
            selection(),
            PermissionMode::Default,
            crate::view::test_workspace(),
        ),
        Some("/Users/dev".into()),
        "0.1.0".into(),
    )
}

fn feed(app: &mut App, seq: u64, event: KernelEvent) {
    app.view.apply(
        SequencedEvent {
            seq,
            revision: 0,
            event,
        },
        seq * 1000,
    );
}

/// 第 `requests` 个请求：输出 `output` 个 token，第一个字之后用了 `generation_ms`。
fn stats(requests: u64, output: u64, generation_ms: u64) -> RequestStats {
    RequestStats {
        requests,
        last: Some(LastRequest {
            output_tokens: Some(output),
            duration_ms: Some(generation_ms + 300),
            first_token_ms: Some(300),
        }),
        ..RequestStats::default()
    }
}

fn card_text(app: &App) -> Vec<String> {
    app.card_layer(100, 0, 0)
        .expect("a card")
        .0
        .iter()
        .map(StyledLine::plain)
        .collect()
}

/// 请求统计的回执记进速度的历史（与 cli 的 Puffin host 同一份 `SpeedHistory`）：回合结束之后的查询给回合收尾，卡的速度
/// 一行有每个回合的数与中位数。离开会话时清空。
#[test]
fn the_speed_row_shows_each_turn_and_the_median() {
    let mut app = app();
    let seq = app.snapshots.request_stats.begin(0);
    assert!(
        app.snapshots
            .request_stats
            .arrived(seq, Some(RequestStats::default()))
    );
    let seq = app.snapshots.request_stats.begin(0);
    app.snapshots
        .request_stats
        .arrived(seq, Some(stats(1, 200, 2000)));
    let seq = app.snapshots.request_stats.begin(1);
    app.snapshots
        .request_stats
        .arrived(seq, Some(stats(1, 200, 2000)));
    let usage = app.stats_usage();
    assert_eq!((usage.last_speed, usage.speeds), (Some(100.0), vec![100.0]));
    app.open_stats();
    let speed = card_text(&app)
        .into_iter()
        .find(|l| l.contains("Speed"))
        .expect("the speed row");
    assert!(speed.contains("tok/s last call"), "{speed}");
    assert!(speed.contains("median"), "{speed}");
    app.snapshots.request_stats.clear();
    assert!(app.stats_usage().speeds.is_empty());
}

/// 主会话里结束了的 Agent 调用：个数、报告的 token 与用时进卡的 Subagents 一行，In all 加上它们的 token（Puffin
/// `subagents::ended`）；在跑的不算。
#[test]
fn ended_subagents_add_the_subagents_and_in_all_rows() {
    let mut app = app();
    let start = |id: &str| KernelEvent::ToolStarted {
        tool_use: ToolUseId::new(id),
        name: "Agent".into(),
        input: json!({"description": "look", "prompt": "p"}),
    };
    feed(&mut app, 1, start("a1"));
    feed(&mut app, 2, start("a2"));
    assert_eq!(app.stats_ended().count, 0, "both still running");
    feed(
        &mut app,
        3,
        KernelEvent::ToolDone {
            tool_use: ToolUseId::new("a1"),
            output: ToolOutput {
                content: vec![ToolResultContent::Text {
                    text: "Done.\n<usage>subagent_tokens: 309000\ntool_uses: 9\nduration_ms: 42000</usage>"
                        .into(),
                }],
                is_error: false,
                display: serde_json::Value::Null,
            },
        },
    );
    let ended = app.stats_ended();
    assert_eq!(
        (ended.count, ended.tokens, ended.work_ms),
        (1, 309000.0, 42000.0)
    );
    app.open_stats();
    let text = card_text(&app);
    assert!(text.iter().any(|l| l.contains("Subagents")), "{text:#?}");
    assert!(text.iter().any(|l| l.contains("In all")), "{text:#?}");
}

/// 目标的卡开着、目标正在跑的这一段有开始时刻时每帧重画（Puffin `goal_moving`）；没有时不重画。
#[test]
fn the_goal_card_moves_only_while_a_stretch_is_running() {
    let mut app = app();
    let goal = |running_since: u64| SessionGoal {
        target_id: String::new(),
        objective: "tests pass".into(),
        title: String::new(),
        status: "active".into(),
        state: GoalState::Working,
        round: 1,
        time_used_seconds: 5,
        running_since,
        rounds: Vec::new(),
        reason: String::new(),
    };
    app.show_goal_card(Some(goal(0)), true);
    assert!(matches!(app.overlay, Some(Overlay::Card(Card::Goal(_)))));
    assert!(!app.cards_fading());
    app.show_goal_card(Some(goal(1_000)), false);
    assert!(app.cards_fading());
}
