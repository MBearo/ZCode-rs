//! 全屏视图共用的工具（tui-kit.md §3.2）：一屏的补齐与页头。
use rust_cli_tui_kit::render::{Span, Style, StyledLine};

use super::*;

fn line(text: &'static str) -> StyledLine {
    StyledLine::new(vec![Span::new(text, Style::DEFAULT)])
}

#[test]
fn fit_pads_and_truncates_to_one_screen() {
    let got = fit(vec![line("abcdef")], 4, 3);
    assert_eq!(got.len(), 3);
    assert_eq!(got[0].plain(), "abc…");
    assert_eq!(got[2], StyledLine::EMPTY);
}

fn ink() -> crate::ui::ink::Ink {
    crate::ui::ink::Ink::new(
        crate::ui::probe::ColorLevel::TrueColor,
        Some((255, 255, 255)),
        Some((0, 0, 0)),
    )
}

/// 头一行的面包屑：line 色的 `  ›  `，中间几级 fg2，最后一级正文墨。
#[test]
fn the_head_draws_crumbs_like_the_settings_page() {
    let ink = ink();
    let got = head(&ink, "Transcript", &["Details"]);
    assert_eq!(got.plain(), "   Transcript  ›  Details");
    assert_eq!(got.spans[2].style, ink.theme.line);
    assert_eq!(got.spans[3].style, Style::DEFAULT);
    let deeper = head(&ink, "Settings", &["Models", "Z.ai"]);
    assert_eq!(deeper.spans[3].style, ink.theme.fg2);
    assert_eq!(deeper.spans[5].style, Style::DEFAULT);
}
