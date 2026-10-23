//! `/usage` 页面（usage.md §4、§5）：会话部分、标签页与范围查询的序号。
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::metering::{ActivityRange, ActivityStats, SessionMetering};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::selection;

use super::{StatsView, UsageTab, UsageView};
use crate::ui::app::{Action, App, Overlay};
use crate::ui::i18n::Lang;
use crate::ui::manage::Manage;
use crate::ui::palette::{ColorLevel, Palette};
use crate::view::Transcript;

fn app() -> App {
    App::new(
        Palette::new(ColorLevel::TrueColor, Some((0, 0, 0))),
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

fn view(app: &App) -> &UsageView {
    match &app.overlay {
        Some(Overlay::Usage(view)) => view,
        other => panic!("no usage page: {other:?}"),
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn the_usage_tab_shows_the_stored_api_duration_first() {
    let mut app = app();
    app.open_usage(
        SessionMetering {
            api_duration_ms: 72_400,
        },
        Ok(ActivityStats::default()),
        false,
    );
    let view = view(&app);
    assert_eq!(view.tab, UsageTab::Usage);
    let lines: Vec<&str> = view.summary.lines().collect();
    assert_eq!(lines[0], "Total duration (API):  1m 12s");
    assert!(lines[1].starts_with("Total duration (wall): "), "{lines:?}");
    assert_eq!(
        lines[2],
        "Total code changes:    0 lines added, 0 lines removed"
    );
    // 还没有用量：零行。
    assert_eq!(
        lines[3],
        "Usage:                 0 input, 0 output, 0 cache read, 0 cache write"
    );
}

#[test]
fn stats_opens_the_stats_tab_and_keys_switch_tabs_and_views() {
    let mut app = app();
    app.open_usage(
        SessionMetering::default(),
        Ok(ActivityStats::default()),
        true,
    );
    assert_eq!(view(&app).tab, UsageTab::Stats);
    app.usage_key(key(KeyCode::Right));
    assert_eq!(view(&app).view, StatsView::Models);
    app.usage_key(key(KeyCode::Left));
    assert_eq!(view(&app).view, StatsView::Overview);
    app.usage_key(key(KeyCode::Tab));
    assert_eq!(view(&app).tab, UsageTab::Usage);
    // Usage 标签页里 ← / → 与 r 不起作用。
    assert_eq!(app.usage_key(key(KeyCode::Char('r'))), None);
    app.usage_key(key(KeyCode::Esc));
    assert!(app.overlay.is_none());
}

#[test]
fn cycling_dates_asks_for_the_range_and_drops_stale_replies() {
    let mut app = app();
    app.open_usage(
        SessionMetering::default(),
        Ok(ActivityStats::default()),
        true,
    );
    assert_eq!(
        app.usage_key(key(KeyCode::Char('r'))),
        Some(Action::Manage(Manage::UsageRange {
            range: ActivityRange::Week,
            seq: 1
        }))
    );
    assert_eq!(
        app.usage_key(key(KeyCode::Char('r'))),
        Some(Action::Manage(Manage::UsageRange {
            range: ActivityRange::Month,
            seq: 2
        }))
    );
    let week = ActivityStats {
        sessions: 7,
        ..ActivityStats::default()
    };
    // 第一次请求的回执晚到：序号不是最新的，丢掉。
    app.usage_range(1, Ok(week.clone()));
    assert_eq!(view(&app).ranged, None);
    app.usage_range(2, Ok(week.clone()));
    assert_eq!(view(&app).ranged, Some(Ok(week)));
    // 回到 All：用已有的全时段结果，不再问内核。
    assert_eq!(app.usage_key(key(KeyCode::Char('r'))), None);
    assert_eq!(view(&app).range, ActivityRange::All);
}
