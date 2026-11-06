//! 「This computer」页：打开、范围的回执按序号收、键（←→ 范围、⇥ 计划的页、esc 回卡、滚动）与页的行。
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::metering::{
    ActivityDay, ActivityRange, ActivityStats, ModelTokens, SessionMetering,
};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::selection;
use rust_cli_tui_kit::app::widgets::usage::{
    DEFAULT_RANGE, Family, PlanKind, UsageModel, UsagePlan,
};

use super::{LocalView, page_body};
use crate::ui::app::{Action, App, Overlay};
use crate::ui::cards::Card;
use crate::ui::i18n::Lang;
use crate::ui::manage::Manage;
use crate::ui::probe::{ColorLevel, Colors};
use crate::ui::usage_plans::PlanAnswer;
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

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn stats() -> ActivityStats {
    ActivityStats {
        range: Some(ActivityRange::All),
        today: "2026-10-18".into(),
        days: vec![
            ActivityDay {
                date: "2026-10-17".into(),
                messages: 4,
            },
            ActivityDay {
                date: "2026-10-18".into(),
                messages: 2,
            },
        ],
        models: vec![ModelTokens {
            model: "GLM-5.3".into(),
            input: 1_200_000,
            output: 300_000,
            cache_read: 0,
            cache_write: 0,
        }],
        sessions: 3,
        longest_session_ms: Some(65 * 60_000),
        ..ActivityStats::default()
    }
}

fn opened() -> App {
    let mut app = app();
    app.composer.set_text("draft");
    app.open_usage(
        SessionMetering {
            api_duration_ms: 12_000,
        },
        Ok(stats()),
    );
    app
}

fn view(app: &App) -> &LocalView {
    match &app.overlay {
        Some(Overlay::Usage(view)) => view,
        other => panic!("not the page: {other:?}"),
    }
}

fn text(app: &App, width: usize) -> (Vec<String>, String) {
    let body = page_body(app, width).expect("the page");
    let lines = body
        .lines
        .iter()
        .map(|l| l.plain().trim_end().to_owned())
        .collect();
    let slot = body
        .slot
        .unwrap()
        .iter()
        .map(|s| s.text.to_string())
        .collect();
    (lines, slot)
}

#[test]
fn the_page_is_the_session_the_heat_map_and_the_totals() {
    let app = opened();
    assert!(app.composer.is_empty(), "the draft is cleared");
    let (lines, slot) = text(&app, 100);
    // 面包屑：没有计划的页时右端什么都没有。
    assert_eq!(lines[1], "   Usage  ›  This computer");
    assert!(lines[3].starts_with("   This session"), "{lines:?}");
    assert!(
        lines[3].ends_with("12s API time  ·  0s wall time"),
        "{lines:?}"
    );
    let all = lines.join("\n");
    assert!(all.contains("Messages, last 52 weeks"), "{all}");
    assert!(
        all.contains("6 total  ·  4 best day  ·  2-day streak"),
        "{all}"
    );
    assert!(all.contains("   Mon  "), "the heat map: {all}");
    assert!(
        all.contains("On this computer   All time  7 days  30 days"),
        "{all}"
    );
    assert!(
        all.contains("   Total       1.5M tokens  ·  3 sessions  ·  2 active days"),
        "{all}"
    );
    assert!(all.contains("   Models      GLM-5.3 1.5M"), "{all}");
    assert!(
        all.contains("   Longest     1h 05m session  ·  2-day streak"),
        "{all}"
    );
    assert_eq!(slot, "←→ range");
}

#[test]
fn ranges_ask_again_and_stale_answers_are_dropped() {
    let mut app = opened();
    let ask = app.usage_key(key(KeyCode::Right));
    assert_eq!(
        ask,
        Some(Action::Manage(Manage::UsageRange {
            range: ActivityRange::Week,
            seq: 1
        }))
    );
    let (lines, _) = text(&app, 100);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("Reading this computer's history")),
        "{lines:?}"
    );
    // 又换了一次：序号 1 的回执晚到，丢掉。
    app.usage_key(key(KeyCode::Right));
    app.usage_range(1, Ok(ActivityStats::default()));
    assert_eq!(view(&app).ranged, None);
    app.usage_range(2, Err("offline".into()));
    let (lines, _) = text(&app, 100);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("Could not read the history: offline")),
        "{lines:?}"
    );
    // 回到全时段不用问。
    assert_eq!(app.usage_key(key(KeyCode::Right)), None);
    assert_eq!(view(&app).range, ActivityRange::All);
}

#[test]
fn tab_goes_to_the_first_plan_page_and_esc_back_to_the_card() {
    let mut app = opened();
    let plan = UsagePlan {
        id: "zai".into(),
        family: Family::Zai,
        kind: PlanKind::Individual,
        name: String::new(),
    };
    app.usage_plans.provider_id = "fixture".into();
    app.plan_answered(
        PlanAnswer::Plans {
            provider: "fixture".into(),
            result: Ok((
                UsageModel {
                    provider_id: "fixture".into(),
                    official: true,
                    ..UsageModel::default()
                },
                vec![plan],
            )),
        },
        0,
    );
    let (lines, slot) = text(&app, 100);
    assert!(lines[1].ends_with("Z.ai plan   This computer"), "{lines:?}");
    assert_eq!(slot, "⇥ plan  ←→ range");
    app.usage_key(key(KeyCode::Tab));
    assert!(matches!(
        &app.overlay,
        Some(Overlay::Card(Card::UsagePage { entry: 0, range, .. })) if *range == DEFAULT_RANGE
    ));
    // 回到这一页，esc 回到卡（再问一次计划，同 Puffin 打开卡）。
    app.open_usage(SessionMetering::default(), Ok(stats()));
    app.usage_key(key(KeyCode::Esc));
    assert!(matches!(
        &app.overlay,
        Some(Overlay::Card(Card::Usage { entry: 0 }))
    ));
}

#[test]
fn the_page_scrolls_within_its_last_frame() {
    let mut app = opened();
    app.page_rows = (10, 25);
    for (code, want) in [
        (KeyCode::Down, 1),
        (KeyCode::PageDown, 10),
        (KeyCode::PageDown, 15),
        (KeyCode::Up, 14),
        (KeyCode::PageUp, 5),
        (KeyCode::PageUp, 0),
    ] {
        app.usage_key(key(code));
        assert_eq!(view(&app).top, want, "{code:?}");
    }
}
