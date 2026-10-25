//! `/goal` 面板与 `goal_status` 行（goal.md §6）：三种面板、三种行、中英文。
use rust_cli_kernel_api::goal::{ActiveGoal, GoalStatus, GoalView};

use super::*;
use crate::ui::i18n::{EN, ZH};
use crate::ui::palette::{ColorLevel, Palette};

fn text(lines: &[Line<'_>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect()
}

fn status(met: bool, failed: bool) -> GoalStatus {
    GoalStatus {
        met,
        sentinel: false,
        failed,
        condition: "tests pass".to_owned(),
        reason: Some("no test runner".to_owned()),
        iterations: (met || failed).then_some(2),
        duration_ms: (met || failed).then_some(65_000),
        tokens: (met || failed).then_some(1_234),
    }
}

fn with_ctx<T>(lang: Lang, run: impl FnOnce(&Ctx<'_>) -> T) -> T {
    let palette = Palette::new(ColorLevel::Ansi16, None);
    let ctx = Ctx {
        palette: &palette,
        lang,
        strings: if lang == Lang::Zh { &ZH } else { &EN },
        width: 80,
        expanded: false,
        elapsed: std::time::Duration::ZERO,
        now_ms: 0,
        workspace: "/Users/dev/project",
        workspace_label: "~/project",
        version: "0.1.0",
        mcp_machines: &[],
        machines: &[],
        markdown: None,
    };
    run(&ctx)
}

#[test]
fn durations_and_tokens_use_the_most_significant_unit() {
    assert_eq!(duration(4_900), "4s");
    assert_eq!(duration(65_000), "1m");
    assert_eq!(duration(7_200_000), "2h");
    assert_eq!(duration(90_000_000), "1d");
    assert_eq!(tokens(999), "999");
    assert_eq!(tokens(1_234), "1.2k");
}

#[test]
fn status_rows_in_both_languages() {
    let en = with_ctx(Lang::En, |ctx| {
        [
            text(&status_lines(&status(false, false), ctx)),
            text(&status_lines(&status(true, false), ctx)),
            text(&status_lines(&status(false, true), ctx)),
        ]
    });
    assert_eq!(
        en[0],
        [format!("{BULLET} Goal not yet met\u{2026} continuing")]
    );
    assert_eq!(
        en[1],
        [format!(
            "{BULLET} Goal achieved (1m \u{b7} 2 turns \u{b7} 1.2k tokens)"
        )]
    );
    assert_eq!(
        en[2],
        [
            format!("{BULLET} Goal could not be achieved (1m \u{b7} 2 turns \u{b7} 1.2k tokens)"),
            "  no test runner".to_owned(),
        ]
    );
    let zh = with_ctx(Lang::Zh, |ctx| {
        text(&status_lines(&status(true, false), ctx))
    });
    assert_eq!(
        zh,
        [format!(
            "{BULLET} 目标已达成（1m \u{b7} 2 轮 \u{b7} 1.2k token）"
        )]
    );
}

#[test]
fn the_three_panels() {
    let active = GoalView {
        active: Some(ActiveGoal {
            condition: "tests pass".to_owned(),
            iterations: 3,
            set_at_ms: 0,
            tokens: 2_500,
            last_reason: Some("  red\nmore detail".to_owned()),
        }),
        achieved: None,
    };
    let lines = with_ctx(Lang::En, |ctx| {
        text(&panel_lines(&panel(&active, Lang::En, 120_000), ctx))
    });
    assert_eq!(
        lines,
        [
            "◉ Goal active",
            "  running 2m \u{b7} 3 turns \u{b7} 2.5k tokens",
            "",
            "  Goal: tests pass",
            "  Last check: red",
            "",
            "  /goal clear to stop early",
        ]
    );
    let achieved = GoalView {
        active: None,
        achieved: Some(status(true, false)),
    };
    let panel_zh = panel(&achieved, Lang::Zh, 0);
    assert_eq!(panel_zh.title, "✓ 目标已达成");
    assert!(panel_zh.success);
    assert_eq!(panel_zh.hint, "/goal <condition> 设定新的目标");
    let none = panel(&GoalView::default(), Lang::En, 0);
    assert_eq!(none.title, "Goal");
    assert_eq!(none.rows, [(None, "No goal set".to_owned())]);
    assert_eq!(none.hint, "/goal <condition> to set one");
}
