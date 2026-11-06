//! 墨色的角色（tui-kit.md §3.4、§3.8）：rich 取 Puffin 的 token，plain（16 色或颜色不明）退到 dim 与 ANSI 色；深浅地不同。
use rust_cli_tui_kit::render::{Color, Style, attr};

use super::*;

const BLACK: (u8, u8, u8) = (0, 0, 0);
const WHITE: (u8, u8, u8) = (255, 255, 255);

fn dark() -> Ink {
    Ink::new(ColorLevel::TrueColor, Some(WHITE), Some(BLACK))
}

fn light() -> Ink {
    Ink::new(ColorLevel::TrueColor, Some(BLACK), Some(WHITE))
}

#[test]
fn a_reporting_truecolor_terminal_gets_the_rich_tokens() {
    let ink = dark();
    assert!(ink.theme.rich && !ink.light);
    assert_eq!(ink.secondary(), ink.theme.fg3);
    assert_eq!(ink.key(), ink.theme.fg2);
    assert_eq!(ink.accent(), ink.theme.brand);
    assert_eq!(ink.selection(), Style::fg(ink.theme.brand));
    assert_eq!(ink.warning(), Style::fg(ink.theme.code));
    assert_eq!(ink.icon_blue(), Style::fg(ink.theme.brand));
    assert_eq!(ink.success(), Style::fg(ink.theme.ok));
    assert_eq!(ink.destructive(), Style::fg(ink.theme.err));
    assert_eq!(ink.context_notice(true), Style::fg(ink.theme.code));
    assert_eq!(ink.context_notice(false), ink.theme.fg2);
    let panel = ink.theme.panel.expect("rich has a panel");
    assert_eq!(ink.user_block(), Style::bg(panel));
    assert_eq!(ink.prompt_block(), Style::bg(panel));
    // 悬停与板不同色，否则在用户消息上看不出（自拟的派生）。
    assert!(ink.hover_bg().is_some_and(|c| c != panel));
    // 选区照 Puffin：底色是 line 的前景色。
    assert_eq!(
        ink.mouse_selection(Style::DEFAULT),
        Style::bg(ink.theme.line.fg_color())
    );
    let current = ink.find_current();
    assert_eq!(current.bg_color(), ink.theme.code);
    assert!(current.has_attr(attr::BOLD) && !current.has_attr(attr::REVERSE));
    assert_eq!(ink.tint((1, 2, 3)), Some(Color::TrueColor(1, 2, 3)));
}

#[test]
fn light_and_dark_grounds_ink_differently() {
    let (d, l) = (dark(), light());
    assert!(l.light);
    assert_ne!(d.secondary(), l.secondary());
    assert_ne!(d.accent(), l.accent());
    assert_ne!(d.user_bg(), l.user_bg());
    // 查看子代理时板的底色：`panel` 向 `agent` 混一点，不等于 `panel`。
    assert_ne!(d.subagent_ground(), d.user_bg());
    assert_ne!(d.subagent_ground(), l.subagent_ground());
}

#[test]
fn sixteen_colours_or_an_unknown_foreground_is_plain() {
    for ink in [
        Ink::new(ColorLevel::Ansi16, Some(WHITE), Some(BLACK)),
        Ink::new(ColorLevel::TrueColor, None, Some(BLACK)),
    ] {
        assert!(!ink.theme.rich);
        assert_eq!(ink.secondary(), Style::DIM);
        assert_eq!(ink.key(), Style::DIM);
        assert_eq!(ink.accent(), Color::BLUE);
        assert_eq!(
            ink.selection(),
            Style::new(Color::BLUE, Color::Default, attr::BOLD)
        );
        assert_eq!(ink.user_bg(), None);
        assert_eq!(ink.hover_bg(), None);
        assert_eq!(ink.mouse_selection(Style::DEFAULT), Style::REVERSE);
        assert!(ink.find_current().has_attr(attr::REVERSE));
        assert_eq!(ink.subagent_ground(), None);
        assert_eq!(ink.tint((1, 2, 3)), None);
    }
    // 只知道背景时，明暗仍按探测到的背景。
    assert!(Ink::new(ColorLevel::Ansi16, None, Some(WHITE)).light);
}

#[test]
fn ink_of_a_palette_takes_the_same_inputs() {
    let colors = Colors::new(ColorLevel::Ansi256, Some(BLACK)).with_foreground(Some(WHITE));
    assert_eq!(
        colors.ink(),
        Ink::new(ColorLevel::Ansi256, Some(WHITE), Some(BLACK))
    );
    // 256 色也是 rich：降采样由渲染器做（tui-kit.md §3.4）。
    assert!(colors.ink().theme.rich);
}
