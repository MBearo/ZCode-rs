//! 状态栏的用例：旧界面的数据经 kit 排出的样子（排版本身的用例在 kit；同画面的比较在 crates/cli/tests/same_picture_s2.rs）。
use rust_cli_tui_kit::render::{Span, Style, StyledLine};
use rust_cli_tui_kit::theme::scheme::ThemeChoice;
use rust_cli_tui_kit::theme::theme::Theme;

use rust_cli_tui_kit::app::widgets::live_speed::LiveSpeed;
use rust_cli_tui_kit::app::widgets::status_bar::{cwd_part, label_parts};

use super::{Part, Window, group, lines, mode_parts, rest};

/// 早已落定的淡入（tick 数）。
const SETTLED: i64 = 1_000_000;

fn plain_theme() -> Theme {
    Theme::resolve(None, None, false, &ThemeChoice::default(), None)
}

fn text(lines: &[StyledLine]) -> Vec<String> {
    lines.iter().map(StyledLine::plain).collect()
}

#[test]
fn the_left_says_who_and_where_and_the_right_says_how_much() {
    let theme = plain_theme();
    let mut left = mode_parts(
        &theme,
        vec![("Ask before changes".to_owned(), theme.fg3)],
        SETTLED,
        true,
    );
    // 模型与档位的部分（kit `label_parts`）：`model · effort`。
    let model = label_parts(&theme, "GLM-5.3", "max")
        .into_iter()
        .map(|(t, s)| Span::new(t, s))
        .collect();
    left.push(Part::new("model", model, 60));
    left.push(cwd_part(&theme, "~/project"));
    let window = Window {
        used: 124_000,
        max: 200_000,
        compact_at: None,
        ..Window::default()
    };
    // 速度是 kit 的现场估算画的（调用之间：量到的数）。
    let speed = LiveSpeed {
        shown: Some(52.4),
        ..LiveSpeed::default()
    }
    .spans(&theme, 0, false);
    let right = group(&theme, Some(window), SETTLED, speed);
    let shown = text(&lines(&theme, &left, &right, &[], 120, 30));
    // 左边从第 3 列起，右边的最后一格后面留一格（Puffin `fit`）。
    let start = "   Ask before changes ⇧⇥  ·  GLM-5.3 · max  ·  ~/project";
    let end = "━━━━━━──── 62%  124k/200k  ·  52 tok/s ";
    let gap = 120 - start.chars().count() - end.chars().count();
    assert_eq!(shown, [format!("{start}{}{end}", " ".repeat(gap))]);
    // 窄时名次低的先走：窗口的大小、速度……百分数留着。
    let narrow = text(&lines(&theme, &left, &right, &[], 50, 10));
    assert!(narrow[0].contains("62%"), "{narrow:?}");
    assert!(!narrow[0].contains("tok/s"), "{narrow:?}");
}

#[test]
fn a_note_takes_the_right_of_the_first_row_and_sends_the_figures_down() {
    let theme = plain_theme();
    let left = mode_parts(&theme, vec![("Ask".to_owned(), theme.fg3)], SETTLED, false);
    let right = group(
        &theme,
        Some(Window {
            used: 10,
            max: 100,
            compact_at: None,
            ..Window::default()
        }),
        SETTLED,
        Vec::new(),
    );
    let note = [Part {
        right: true,
        ..Part::new("note", vec![Span::new("Copied", Style::DEFAULT)], 95)
    }];
    let shown = text(&lines(&theme, &left, &right, &note, 40, 30));
    assert_eq!(shown.len(), 2, "{shown:?}");
    assert!(shown[0].ends_with("Copied "), "{shown:?}");
    assert!(shown[1].starts_with("   ━"), "{shown:?}");
}

#[test]
fn ready_machines_follow_the_folder_and_leave_first_when_narrow() {
    let theme = plain_theme();
    let mut left = mode_parts(&theme, vec![("Ask".to_owned(), theme.fg3)], SETTLED, true);
    left.extend(rest(
        &theme,
        Some(("GLM-5.3", "max", SETTLED)),
        "~/project",
        Some("@dev-a +1"),
    ));
    let wide = text(&lines(&theme, &left, &[], &[], 80, 30));
    assert_eq!(
        wide[0].trim_end(),
        "   Ask ⇧⇥  ·  GLM-5.3 · max  ·  ~/project  ·  @dev-a +1"
    );
    let narrow = text(&lines(&theme, &left, &[], &[], 44, 30));
    assert!(!narrow[0].contains("@dev-a"), "{narrow:?}");
    assert!(narrow[0].contains("~/project"), "{narrow:?}");
}
