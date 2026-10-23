//! 按键提示组件（tui.md §7.4，修复线 F26）：按键与说明分开上样式，深浅背景、中英文。
use ratatui::style::{Color, Modifier};

use super::*;
use crate::ui::palette::ColorLevel;

fn dark() -> Palette {
    Palette::new(ColorLevel::TrueColor, Some((0, 0, 0))).with_foreground(Some((255, 255, 255)))
}

#[test]
fn keys_are_bright_and_bold_and_the_rest_is_gray() {
    let spans = spans("{enter} to select · {esc} to cancel", &dark());
    let shown: Vec<(&str, bool)> = spans
        .iter()
        .map(|s| {
            (
                s.content.as_ref(),
                s.style.add_modifier.contains(Modifier::BOLD),
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            ("enter", true),
            (" to select · ", false),
            ("esc", true),
            (" to cancel", false)
        ]
    );
    assert_eq!(spans[1].style.fg, Some(Color::Rgb(153, 153, 153)));
    assert_eq!(spans[0].style.fg, None, "the terminal's own foreground");
    // 浅色背景：按键也按前景与背景混合。
    let light =
        Palette::new(ColorLevel::TrueColor, Some((255, 255, 255))).with_foreground(Some((0, 0, 0)));
    let spans = super::spans("{esc} 关闭", &light);
    assert_eq!(spans[0].style.fg, Some(Color::Rgb(38, 38, 38)));
    assert_eq!(spans[1].content, " 关闭");
    assert_eq!(spans[1].style.fg, Some(Color::Rgb(102, 102, 102)));
}

#[test]
fn templates_without_marks_are_plain_secondary_text() {
    assert_eq!(plain("{↑/↓} 移动 · {enter} 打开"), "↑/↓ 移动 · enter 打开");
    assert_eq!(plain("no keys {here"), "no keys {here");
    let unknown = Palette::new(ColorLevel::TrueColor, Some((0, 0, 0)));
    let spans = spans("{?} for shortcuts", &unknown);
    // 不知道前景：说明 dim，按键粗体。
    assert!(spans[1].style.add_modifier.contains(Modifier::DIM));
    assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(
        line(2, "{?} for shortcuts", &unknown).spans[0].content,
        "  "
    );
}
