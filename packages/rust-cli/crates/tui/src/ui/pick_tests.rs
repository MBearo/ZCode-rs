//! 选择列表的一行（tui.md §7.4，修复线 F26）：底色条铺满整行、名字粗体、说明不加粗；16 色反色。
use ratatui::style::{Color, Modifier, Style};

use super::*;
use crate::ui::palette::ColorLevel;

fn row(palette: &Palette) -> Line<'static> {
    Line::from(vec![
        pointer(true),
        Span::raw("build"),
        Span::raw("   "),
        Span::styled("Ask before changes", palette.secondary()),
    ])
}

#[test]
fn the_selected_row_is_a_full_width_bar() {
    let palette =
        Palette::new(ColorLevel::TrueColor, Some((0, 0, 0))).with_foreground(Some((255, 255, 255)));
    let line = paint(row(&palette), true, &palette, 40);
    assert_eq!(line.width(), 40);
    assert_eq!(line.spans[0].content, "› ");
    let bar = Some(Color::Rgb(99, 168, 248));
    assert!(
        line.spans.iter().all(|span| span.style.bg == bar),
        "{line:?}"
    );
    assert_eq!(line.style.bg, bar);
    assert!(line.spans[1].style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(line.spans[1].style.fg, Some(Color::Rgb(0, 0, 46)));
    // 说明不加粗、不 dim。
    let description = &line.spans[3].style;
    assert!(!description.add_modifier.contains(Modifier::BOLD));
    assert!(!description.add_modifier.contains(Modifier::DIM));
    // 没选中：原样（说明是灰色）。
    let plain = paint(row(&palette), false, &palette, 40);
    assert_eq!(plain.spans[3].style.fg, Some(Color::Rgb(153, 153, 153)));
    assert_eq!(pointer(false).content, "  ");
}

#[test]
fn light_backgrounds_use_the_lighter_blue_and_sixteen_colors_reverse() {
    let light =
        Palette::new(ColorLevel::TrueColor, Some((255, 255, 255))).with_foreground(Some((0, 0, 0)));
    let line = paint(row(&light), true, &light, 30);
    assert_eq!(line.spans[1].style.bg, Some(Color::Rgb(164, 205, 251)));
    let ansi = Palette::new(ColorLevel::Ansi16, Some((0, 0, 0)));
    let line = paint(row(&ansi), true, &ansi, 30);
    assert_eq!(line.width(), 30);
    assert!(
        line.spans
            .iter()
            .all(|s| s.style.add_modifier.contains(Modifier::REVERSED))
    );
    assert!(line.spans[1].style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(line.style, Style::default());
}
