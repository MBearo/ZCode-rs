//! 按键提示组件（tui.md §7.4，修复线 F26；墨色 tui-kit.md §3.8）：按键与说明分开上墨色，深浅地、plain、中英文。
use rust_cli_tui_kit::render::Style;

use super::*;
use crate::ui::probe::ColorLevel;

fn dark() -> Ink {
    Ink::new(
        ColorLevel::TrueColor,
        Some((255, 255, 255)),
        Some((0, 0, 0)),
    )
}

#[test]
fn keys_take_fg2_and_the_rest_fg3() {
    let ink = dark();
    let spans = spans("{enter} to select · {esc} to cancel", &ink);
    let shown: Vec<(&str, Style)> = spans.iter().map(|s| (s.text.as_ref(), s.style)).collect();
    assert_eq!(
        shown,
        [
            ("enter", ink.theme.fg2),
            (" to select · ", ink.theme.fg3),
            ("esc", ink.theme.fg2),
            (" to cancel", ink.theme.fg3)
        ]
    );
    // 浅色地：同样的角色，墨色随地变。
    let light = Ink::new(
        ColorLevel::TrueColor,
        Some((0, 0, 0)),
        Some((255, 255, 255)),
    );
    let spans = super::spans("{esc} 关闭", &light);
    assert_eq!(spans[0].style, light.theme.fg2);
    assert_ne!(spans[0].style, ink.theme.fg2);
    assert_eq!(spans[1].text, " 关闭");
    assert_eq!(spans[1].style, light.theme.fg3);
}

#[test]
fn templates_without_marks_are_plain_secondary_text() {
    assert_eq!(plain("{↑/↓} 移动 · {enter} 打开"), "↑/↓ 移动 · enter 打开");
    assert_eq!(plain("no keys {here"), "no keys {here");
    // 不知道前景：Puffin 的 plain 主题，键与说明都 dim。
    let unknown = Ink::new(ColorLevel::TrueColor, None, Some((0, 0, 0)));
    let spans = spans("{?} for shortcuts", &unknown);
    assert_eq!(spans[0].style, Style::DIM);
    assert_eq!(spans[1].style, Style::DIM);
}
