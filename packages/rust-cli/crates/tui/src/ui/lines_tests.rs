//! 折行与截断（tui-kit.md §3.6）：原来 `text.rs` 的用例，另加续行的 `cont`。
use rust_cli_tui_kit::render::{Continuation, Span, Style, StyledLine, attr};

use super::*;

fn texts(lines: &[StyledLine]) -> Vec<String> {
    lines.iter().map(plain).collect()
}

#[test]
fn words_wrap_under_a_hanging_prefix() {
    let lines = wrap(
        &[Span::plain("hello wide world")],
        9,
        &[Span::plain("• ")],
        &[Span::plain("  ")],
    );
    assert_eq!(texts(&lines), ["• hello ", "  wide ", "  world"]);
}

#[test]
fn long_words_and_wide_characters_break_anywhere() {
    let lines = wrap(&[Span::plain("abcdefghij")], 4, &[], &[]);
    assert_eq!(texts(&lines), ["abcd", "efgh", "ij"]);
    let lines = wrap(
        &[Span::plain("你好世界")],
        6,
        &[Span::plain("› ")],
        &[Span::plain("  ")],
    );
    assert_eq!(texts(&lines), ["› 你好", "  世界"]);
}

#[test]
fn styles_survive_and_text_keeps_its_line_breaks() {
    let bold = Style::BOLD;
    let lines = wrap(&[Span::plain("a "), Span::new("bold", bold)], 3, &[], &[]);
    assert_eq!(texts(&lines), ["a ", "bol", "d"]);
    assert_eq!(lines[1].spans[0].style, bold);
    let lines = wrap_text(
        "one\ntwo",
        Style::DEFAULT,
        10,
        &[Span::plain("└ ")],
        &[Span::plain("  ")],
    );
    assert_eq!(texts(&lines), ["└ one", "  two"]);
    // 原有的换行不是软折行。
    assert!(lines.iter().all(|l| l.cont.is_none()));
}

#[test]
fn truncation_counts_display_width() {
    assert_eq!(truncate("abcdef", 4), "abc…");
    assert_eq!(truncate("你好世界", 5), "你好…");
    assert_eq!(truncate("abc", 4), "abc");
    let line = truncate_line(
        StyledLine::new(vec![Span::plain("ab"), Span::plain("cdef")]),
        4,
    );
    assert_eq!(plain(&line), "abc…");
    assert_eq!(width(&line.spans), 4);
}

/// 续行的 `cont`：文字从续行前缀的宽度开始，折行处吃掉空格的行标出来，硬断的不标（tui-select.md §2.3，原来经
/// `text::wrap` 的零宽记号）。
#[test]
fn continuation_rows_mark_the_indent_and_the_eaten_space() {
    let spans = [Span::new(
        "abcdefg hi wide world abcdefghijkl",
        Style::ITALIC,
    )];
    let first = [Span::plain("• ")];
    let rest = [Span::plain("  ")];
    let new = wrap(&spans, 9, &first, &rest);
    let rows: Vec<_> = new.iter().map(|line| (plain(line), line.cont)).collect();
    let cont = |space| Some(Continuation { at: 2, space });
    assert_eq!(
        rows,
        [
            ("• abcdefg".to_owned(), None),
            ("  hi wide".to_owned(), cont(true)),
            ("  world ".to_owned(), cont(true)),
            ("  abcdefg".to_owned(), cont(false)),
            ("  hijkl".to_owned(), cont(false)),
        ]
    );
    // 样式与 decor 跟着字走。
    assert!(new[1].spans[1].style.has_attr(attr::ITALIC));
}

/// 宽度来自 kit 的表：emoji 缺省宽 2（tui-kit.md §3.6）。
#[test]
fn widths_come_from_the_kit_table() {
    assert_eq!(width(&[Span::plain("✅a")]), 3);
    assert_eq!(
        texts(&wrap(&[Span::plain("✅✅✅")], 4, &[], &[])),
        ["✅✅", "✅"]
    );
}
