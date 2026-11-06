//! 档位控件的 `auto` 一档（docs/specs/rust-cli/tui-kit.md §4.2 对应表的 `/model` 一行，用户决定 2026-10-31）。
use crate::render::Style;
use crate::theme::rgb::Rgb;
use crate::theme::theme::Theme;

use super::{AUTO, effort_bars};

fn theme() -> Theme {
    Theme::from(Rgb::new(214, 216, 220), Rgb::new(21, 22, 26))
}

fn levels(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

/// 条的文字（两格一根）与它亮不亮。
fn bars(theme: &Theme, efforts: &[String], idx: usize) -> Vec<(String, bool)> {
    effort_bars(theme, efforts, Some(idx), 99)
        .into_iter()
        .filter(|span| span.text.trim() != "" && span.text.chars().all(|c| "▁▂▃▄▅▆▇█".contains(c)))
        .map(|span| (span.text.into_owned(), span.style == Style::fg(theme.brand)))
        .collect()
}

#[test]
fn auto_has_no_bar_and_lights_nothing() {
    let theme = theme();
    let with = levels(&[AUTO, "low", "medium", "high"]);
    let without = levels(&["low", "medium", "high"]);
    // `auto` 没有条：条与没有 `auto` 时一样高。
    let heights = |b: Vec<(String, bool)>| b.into_iter().map(|(t, _)| t).collect::<Vec<_>>();
    assert_eq!(
        heights(bars(&theme, &with, 0)),
        heights(bars(&theme, &without, 0))
    );
    // 选中 `auto`：所有条都暗，名字是 `auto`。
    assert!(bars(&theme, &with, 0).iter().all(|(_, on)| !on));
    let text: String = effort_bars(&theme, &with, Some(0), 99)
        .iter()
        .map(|span| span.text.as_ref())
        .collect();
    assert!(text.ends_with(AUTO), "{text:?}");
    // 选中 medium：low 与 medium 亮，high 暗。
    let lit: Vec<bool> = bars(&theme, &with, 2)
        .into_iter()
        .map(|(_, on)| on)
        .collect();
    assert_eq!(lit, [true, true, false]);
}

/// 不认识的档位按位置算条高，打头的 `auto` 不算一个位置。
#[test]
fn auto_does_not_shift_unknown_levels() {
    let theme = theme();
    let with = levels(&[AUTO, "fast", "deep"]);
    let without = levels(&["fast", "deep"]);
    let heights = |b: Vec<(String, bool)>| b.into_iter().map(|(t, _)| t).collect::<Vec<_>>();
    assert_eq!(
        heights(bars(&theme, &with, 1)),
        heights(bars(&theme, &without, 0))
    );
}
