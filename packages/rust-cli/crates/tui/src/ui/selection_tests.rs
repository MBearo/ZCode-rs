use rust_cli_tui_kit::render::{Continuation, Span, Style, StyledLine};

use super::*;
use crate::ui::lines::wrap_text;
use crate::ui::probe::ColorLevel;

fn row(text: &str, soft: Option<Continuation>) -> Row {
    Row {
        text: text.to_owned(),
        soft,
    }
}

#[test]
fn soft_wrapped_rows_join_without_a_newline() {
    let rows = [
        row("⏺ first part of the error,", None),
        row("  second part", Some(Continuation { at: 2, space: true })),
        row("next line  ", None),
    ];
    assert_eq!(
        join(&rows, 2, usize::MAX),
        "first part of the error, second part\nnext line"
    );
}

#[test]
fn a_hard_broken_word_joins_without_a_space() {
    let rows = [
        row("  req_0123", None),
        row(
            "  456789",
            Some(Continuation {
                at: 2,
                space: false,
            }),
        ),
    ];
    assert_eq!(join(&rows, 2, usize::MAX), "req_0123456789");
}

/// kit 的行经 `cont` 接回原文（tui-kit.md §3.1：不再从样式里解记号）。
#[test]
fn wrapping_and_joining_gives_back_the_original_text() {
    let text = "InvokeModelWithResponseStream: operation error Bedrock Runtime: https response error StatusCode: 400, RequestID: 0f0e0d0c-1111-2222-3333-444455556666";
    let first = [Span::plain("⏺ ")];
    let rest = [Span::plain("  ")];
    for width in [20, 33, 47, 80] {
        let lines = wrap_text(text, Style::DEFAULT, width, &first, &rest);
        assert!(lines.len() > 1 || width == 80);
        let rows: Vec<Row> = lines
            .iter()
            .map(|line| Row {
                text: line.plain(),
                soft: line.cont,
            })
            .collect();
        assert_eq!(join(&rows, 2, usize::MAX), text, "width {width}");
    }
}

#[test]
fn words_follow_the_character_classes() {
    let text = "see foo/bar.baz-1 now";
    // 双击 `bar`：路径里的 `/ . -` 与字母同一类。
    assert_eq!(word_at(text, 9), (4, 17));
    // 双击空白：连续的空白。
    assert_eq!(word_at("a   b", 2), (1, 4));
    // 其他符号自成一类。
    assert_eq!(word_at("x=(y)", 2), (1, 3));
}

/// 鼠标选区：rich 时只换底色（`line` 的前景色）、前景不变；plain 反显（tui-select.md §2.2，tui-kit.md §3.4）。
#[test]
fn the_mouse_selection_changes_only_the_background() {
    let ink = Ink::new(
        ColorLevel::TrueColor,
        Some((220, 220, 220)),
        Some((20, 20, 20)),
    );
    let red = ink.destructive();
    let mut line = StyledLine::new(vec![Span::new("hello", red)]);
    mark_line(&mut line, 1, 3, &ink);
    assert_eq!(line.plain(), "hello");
    let texts: Vec<&str> = line.spans.iter().map(|s| s.text.as_ref()).collect();
    assert_eq!(texts, ["h", "el", "lo"]);
    let bg = ink.selection_bg().unwrap();
    assert_eq!(line.spans[1].style, red.with_bg(bg));
    assert_eq!(line.spans[0].style, red);
    let plain = Ink::new(ColorLevel::Ansi16, None, None);
    let mut line = StyledLine::new(vec![Span::plain("hello")]);
    mark_line(&mut line, 0, 2, &plain);
    assert_eq!(line.spans[0].style, Style::REVERSE);
}
