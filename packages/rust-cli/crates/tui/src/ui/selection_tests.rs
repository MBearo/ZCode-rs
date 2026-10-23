use super::*;
use crate::ui::text::{SoftWrap, plain, soft_wrap, wrap_text};

fn row(text: &str, soft: Option<SoftWrap>) -> Row {
    Row {
        text: text.to_owned(),
        soft,
    }
}

#[test]
fn soft_wrapped_rows_join_without_a_newline() {
    let rows = [
        row("⏺ first part of the error,", None),
        row(
            "  second part",
            Some(SoftWrap {
                indent: 2,
                space: true,
            }),
        ),
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
            Some(SoftWrap {
                indent: 2,
                space: false,
            }),
        ),
    ];
    assert_eq!(join(&rows, 2, usize::MAX), "req_0123456789");
}

#[test]
fn wrapping_and_joining_gives_back_the_original_text() {
    let text = "InvokeModelWithResponseStream: operation error Bedrock Runtime: https response error StatusCode: 400, RequestID: 0f0e0d0c-1111-2222-3333-444455556666";
    let first = [ratatui::text::Span::raw("⏺ ")];
    let rest = [ratatui::text::Span::raw("  ")];
    for width in [20, 33, 47, 80] {
        let lines = wrap_text(text, Style::default(), width, &first, &rest);
        assert!(lines.len() > 1 || width == 80);
        let rows: Vec<Row> = lines
            .iter()
            .map(|line| row(&plain(line), soft_wrap(line)))
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

#[test]
fn highlight_changes_only_the_background_and_keeps_markers() {
    let mut line = ratatui::text::Line::from(vec![
        ratatui::text::Span::styled("hello", Style::default().fg(ratatui::style::Color::Red)),
        ratatui::text::Span::raw(""),
    ]);
    highlight(&mut line, 1, 3, ratatui::style::Color::Blue);
    assert_eq!(plain(&line), "hello");
    assert_eq!(line.spans.len(), 4);
    assert_eq!(line.spans[1].content, "el");
    assert_eq!(line.spans[1].style.bg, Some(ratatui::style::Color::Blue));
    assert_eq!(line.spans[1].style.fg, Some(ratatui::style::Color::Red));
    assert_eq!(line.spans[3].content, "");
}
