//! `goal_status` 行（goal.md §6）：三种行、中英文。`/goal` 的卡在 cards.rs。
use rust_cli_kernel_api::goal::GoalStatus;

use rust_cli_tui_kit::render::attr;

use super::*;
use crate::ui::probe::{ColorLevel, Colors};

fn text(lines: &[StyledLine]) -> Vec<String> {
    lines.iter().map(StyledLine::plain).collect()
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
    let colors = Colors::new(ColorLevel::Ansi16, None);
    let ink = colors.ink();
    let ctx = Ctx {
        ink: &ink,
        lang,
        width: 80,
        expanded: false,
        elapsed: std::time::Duration::ZERO,
        now_ms: 0,
        workspace: "/Users/dev/project",
        workspace_label: "~/project",
        version: "0.1.0",
        mcp_machines: &[],
        skills: &[],
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
    assert_eq!(en[0], [" ◎ Goal not yet met\u{2026} continuing"]);
    assert_eq!(
        en[1],
        [" ✓ Goal achieved (1m \u{b7} 2 turns \u{b7} 1.2k tokens)"]
    );
    assert_eq!(
        en[2],
        [
            " ✗ Goal could not be achieved (1m \u{b7} 2 turns \u{b7} 1.2k tokens)",
            "   │ no test runner",
        ]
    );
    // 记号与文字的墨（Puffin 的检查行，tui-kit.md §4.2「`/goal`」）：达成 ok 的 ✓，失败 err 的 ✗，继续 brand 的 ◎；句子
    // 达成与失败 fg2 粗体，继续 fg3；统计 fg3；理由在 line 色的竖线后面，fg2。
    with_ctx(Lang::En, |ctx| {
        let theme = &ctx.ink.theme;
        let row = |s: GoalStatus| status_lines(&s, ctx);
        let met = row(status(true, false));
        assert_eq!(met[0].spans[1].style, Style::fg(theme.ok));
        assert_eq!(met[0].spans[3].style, theme.fg2.with_attr(attr::BOLD));
        assert_eq!(met[0].spans[4].style, theme.fg3);
        let failed = row(status(false, true));
        assert_eq!(failed[0].spans[1].style, Style::fg(theme.err));
        assert_eq!(failed[1].spans[1].style, theme.line);
        assert_eq!(failed[1].spans[3].style, theme.fg2);
        let going = row(status(false, false));
        assert_eq!(going[0].spans[1].style, Style::fg(theme.brand));
        assert_eq!(going[0].spans[3].style, theme.fg3);
    });
    let zh = with_ctx(Lang::Zh, |ctx| {
        text(&status_lines(&status(true, false), ctx))
    });
    assert_eq!(zh, [" ✓ 目标已达成（1m \u{b7} 2 轮 \u{b7} 1.2k token）"]);
}
