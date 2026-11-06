//! 布局的测试。
//! 移植自 packages/tui/tui/test/src/zcode/tui/app/LayoutSuite.scala（feat/tui a2bf878b34）；DesignLayoutSuite 与
//! WrapSuite 在子模块里。最后几个测试是 Rust 自己的（记忆的身份、整帧移出、光标、表格）。
mod design;
mod wrap;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::render::{Color, Span, Style, StyledLine, ansi};

use super::element::{
    ClipFocus, ColumnAlign, Element, Wrap, br, by_width, caret_at, clip_rows, colored, empty, hr,
    labelled_hr, memo, spinner, stack, styled_text, table, text, wrap_text,
};
use super::layout::Layout;

fn plain(ls: &[StyledLine]) -> Vec<String> {
    ls.iter().map(StyledLine::plain).collect()
}

fn lay_plain(e: &Element, w: usize) -> Vec<String> {
    plain(&Layout::lay(e, w))
}

#[test]
fn a_single_styled_span_renders_as_set_sequence_text_reset() {
    // 壳直接用这个串造输入框的光标格，而不是 text(_).style(_).render()；两者必须逐字节相同。
    assert_eq!(
        format!("{}x{}", Style::UNDERLINE.set_sequence(), ansi::RESET),
        text("x").style(Style::UNDERLINE).render()
    );
}

#[test]
fn text_becomes_one_styled_line_newlines_split_into_rows() {
    assert_eq!(
        Layout::lay(&text("hi"), 10),
        vec![StyledLine::new(vec![Span::new("hi", Style::DEFAULT)])]
    );
    assert_eq!(lay_plain(&text("a\nb"), 10), vec!["a", "b"]);
}

#[test]
fn stack_stacks_children_br_is_a_blank_line_empty_contributes_nothing() {
    let e = stack([text("a"), br(), empty(), text("b")]);
    assert_eq!(lay_plain(&e, 10), vec!["a", "", "b"]);
}

#[test]
fn hr_expands_to_the_layout_width() {
    assert_eq!(lay_plain(&hr('-', Color::Default), 5), vec!["-----"]);
}

#[test]
fn labelled_hr_centers_its_label_in_a_full_width_rule() {
    let e = labelled_hr("hey", Style::DEFAULT);
    assert_eq!(lay_plain(&e, 15)[0], "───── hey ─────");
    // 填充是奇数时多的一格在右边。
    assert_eq!(lay_plain(&e, 14)[0], "──── hey ─────");
    // 随宽度重排：没有烤进任何东西（resize 重画的不变量）。
    assert_eq!(lay_plain(&e, 9)[0], "── hey ──");
}

#[test]
fn labelled_hr_degrades_to_the_bare_label_when_the_line_is_too_narrow() {
    let e = labelled_hr("hey", Style::DEFAULT);
    assert_eq!(lay_plain(&e, 6)[0], "hey");
    assert_eq!(lay_plain(&e, 2)[0], "he");
}

#[test]
fn color_application_and_style_attach_the_right_style() {
    let cyan = &Layout::lay(&colored(Color::CYAN, "x"), 10)[0];
    assert_eq!(cyan.spans, vec![Span::new("x", Style::fg(Color::CYAN))]);
    let bold_cyan = &Layout::lay(&colored(Color::CYAN, "x").style(Style::BOLD), 10)[0];
    assert_eq!(
        bold_cyan.spans,
        vec![Span::new("x", Style::fg(Color::CYAN).merge(Style::BOLD))]
    );
}

#[test]
fn embedded_ansi_in_a_text_string_is_re_tokenized_into_styled_spans() {
    // 应用用子元素的 render 拼字符串；布局要把段找回来。
    let s = colored(Color::CYAN, "›").render() + " hello";
    let line = &Layout::lay(&text(s), 40)[0];
    assert_eq!(
        line.spans,
        vec![
            Span::new("›", Style::fg(Color::CYAN)),
            Span::new(" hello", Style::DEFAULT)
        ]
    );
}

#[test]
fn reverse_video_cursor_cell_tokenizes_to_a_reverse_span() {
    let cell = text("X").style(Style::REVERSE).render(); // ESC[0;7mXESC[0m
    let line = &Layout::lay(&text(format!("> {cell}")), 40)[0];
    assert_eq!(
        line.spans,
        vec![
            Span::new("> ", Style::DEFAULT),
            Span::new("X", Style::REVERSE)
        ]
    );
}

#[test]
fn spinner_renders_a_glyph_followed_by_the_label() {
    let line = &Layout::lay(&spinner("waiting", 0), 20)[0];
    assert!(line.plain().ends_with(" waiting"), "{}", line.plain());
    assert_eq!(line.plain().chars().count(), "waiting".len() + 2); // 字形 + 空格 + 标签
}

#[test]
fn wrapped_text_uses_first_and_continuation_prefixes() {
    let lines = Layout::lay(&wrap_text("> ", "  ", "abcdef", Wrap::Word), 5);
    assert_eq!(plain(&lines), vec!["> abc", "  def"]);
    assert!(lines.iter().all(|l| l.width() <= 5), "{:?}", plain(&lines));
}

#[test]
fn wrapped_text_preserves_embedded_ansi_styles_across_rows() {
    let cursor = text("c").style(Style::REVERSE).render();
    let lines = Layout::lay(
        &wrap_text("> ", "  ", format!("ab{cursor}def"), Wrap::Word),
        5,
    );
    assert_eq!(plain(&lines), vec!["> abc", "  def"]);
    assert_eq!(
        lines[0].spans,
        vec![
            Span::new("> ", Style::DEFAULT),
            Span::new("ab", Style::DEFAULT),
            Span::new("c", Style::REVERSE)
        ]
    );
}

#[test]
fn wrapped_text_treats_explicit_newlines_as_row_breaks() {
    let e = wrap_text("> ", "  ", "ab\ncd", Wrap::Word);
    assert_eq!(lay_plain(&e, 10), vec!["> ab", "  cd"]);
}

/* ---- clip_rows（输入框的高度上限） ---- */

/// `n` 个一行的 `l1..ln`，第 `cursor` 行（0 起）是光标样式：[`ClipFocus::Cursor`] 围着它开窗。
fn tall_stack(n: usize, cursor: Option<usize>) -> Element {
    stack((0..n).map(|i| {
        let t = text(format!("l{}", i + 1));
        if Some(i) == cursor {
            t.style(Style::CARET)
        } else {
            t
        }
    }))
}

fn ls(range: std::ops::RangeInclusive<usize>) -> Vec<String> {
    range.map(|i| format!("l{i}")).collect()
}

#[test]
fn clip_rows_passes_short_content_through_untouched() {
    let e = clip_rows(tall_stack(12, None), 12, ClipFocus::Head);
    assert_eq!(lay_plain(&e, 20), ls(1..=12));
}

#[test]
fn clip_rows_head_keeps_the_first_rows_and_a_counted_marker() {
    let lines = lay_plain(&clip_rows(tall_stack(30, None), 12, ClipFocus::Head), 30);
    assert_eq!(lines.len(), 12);
    assert_eq!(lines[..11], ls(1..=11)[..]);
    assert_eq!(lines[11], "… 19 more lines");
}

#[test]
fn clip_rows_head_appends_the_hint_to_the_marker() {
    let e: Element = super::element::Clip::new(tall_stack(30, None), 12, ClipFocus::Head)
        .with_hint("see transcript")
        .into();
    assert_eq!(
        lay_plain(&e, 40).last().map(String::as_str),
        Some("… 19 more lines (see transcript)")
    );
}

#[test]
fn clip_rows_cursor_centers_the_window_on_the_cursor_row() {
    // 光标在 l15（第 14 行）：窗口是第 10..19 行，两侧各藏十行，光标行在窗口中间。
    let lines = lay_plain(
        &clip_rows(tall_stack(30, Some(14)), 12, ClipFocus::Cursor),
        30,
    );
    assert_eq!(lines.len(), 12);
    assert_eq!(lines[0], "… 10 more lines");
    assert_eq!(lines[11], "… 10 more lines");
    assert_eq!(lines[1..11], ls(11..=20)[..]);
}

#[test]
fn clip_rows_cursor_pins_to_the_top_edge_with_a_single_bottom_marker() {
    let lines = lay_plain(
        &clip_rows(tall_stack(30, Some(1)), 12, ClipFocus::Cursor),
        30,
    );
    assert_eq!(lines[..11], ls(1..=11)[..]);
    assert_eq!(lines[11], "… 19 more lines");
}

#[test]
fn clip_rows_cursor_pins_to_the_bottom_edge_with_a_single_top_marker() {
    let lines = lay_plain(
        &clip_rows(tall_stack(30, Some(29)), 12, ClipFocus::Cursor),
        30,
    );
    assert_eq!(lines[0], "… 19 more lines");
    assert_eq!(lines[1..], ls(20..=30)[..]);
}

#[test]
fn clip_rows_cursor_falls_back_to_the_last_row_when_no_cursor_cell_is_laid() {
    let lines = lay_plain(&clip_rows(tall_stack(30, None), 12, ClipFocus::Cursor), 30);
    assert_eq!(lines[0], "… 19 more lines");
    assert_eq!(lines[11], "l30");
}

#[test]
fn clip_rows_cursor_windows_around_the_caret_not_an_earlier_reverse_or_underline_span() {
    // l2 上的链接（下划线）与 l4 上的选区（反显）都在 l25 的光标之前。
    let rows = (0..30).map(|i| {
        let t = format!("l{}", i + 1);
        match i {
            1 => styled_text(t, Style::UNDERLINE),
            3 => styled_text(t, Style::REVERSE),
            24 => styled_text(t, Style::CARET),
            _ => text(t),
        }
    });
    let lines = lay_plain(&clip_rows(stack(rows), 12, ClipFocus::Cursor), 30);
    assert!(lines.iter().any(|l| l == "l25"), "{}", lines.join("\n"));
    assert!(!lines.iter().any(|l| l == "l2"), "{}", lines.join("\n"));
}

#[test]
fn clip_rows_markers_use_the_singular_for_one_hidden_line() {
    // 光标在 l6：居中的窗口从第 1 行开始，正好藏 l1。
    let lines = lay_plain(
        &clip_rows(tall_stack(30, Some(5)), 12, ClipFocus::Cursor),
        30,
    );
    assert_eq!(lines[0], "… 1 more line");
}

#[test]
fn clip_rows_markers_are_dim_rows_truncated_to_the_width() {
    let lines = Layout::lay(&clip_rows(tall_stack(30, None), 12, ClipFocus::Head), 8);
    assert_eq!(
        lines.last().map(|l| l.spans.clone()),
        Some(vec![Span::new("… 19 mor", Style::DIM)])
    );
}

/* ---- Rust 自己的测试 ---- */

#[test]
fn ends_focus_keeps_head_and_tail_around_one_marker() {
    let e = clip_rows(
        tall_stack(30, None),
        8,
        ClipFocus::Ends { head: 4, tail: 3 },
    );
    let lines = lay_plain(&e, 30);
    assert_eq!(lines[..4], ls(1..=4)[..]);
    assert_eq!(lines[4], "… 23 more lines");
    assert_eq!(lines[5..], ls(28..=30)[..]);
}

#[test]
fn a_memo_lays_once_per_width_and_shares_its_lines() {
    // 渲染器的测试会并发地采纳类宽度（进程全局的代数加一），记忆因此合法地重铺：代数在中途变了就重来。
    for _ in 0..8 {
        let generation = crate::render::width::generation();
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        let m = memo(by_width(move |w| {
            seen.fetch_add(1, Ordering::SeqCst);
            text("x".repeat(w.min(3)))
        }));
        let Element::Memo(node) = &m else {
            panic!("memo builds a Memo node")
        };
        let a = Layout::lay_memo(node, 10);
        let b = Layout::lay_memo(node, 10);
        let both = Layout::lay(&stack([m.clone(), m.clone()]), 10);
        let hits = calls.load(Ordering::SeqCst);
        // 换了宽度就重铺（一格，覆盖）。
        let narrow = lay_plain(&m, 2);
        let after = calls.load(Ordering::SeqCst);
        if crate::render::width::generation() != generation {
            continue;
        }
        assert!(Arc::ptr_eq(&a, &b), "a hit replays the same lines");
        assert_eq!(both.len(), 2);
        assert_eq!(hits, 1);
        assert_eq!(narrow, vec!["xx"]);
        assert_eq!(after, 2);
        return;
    }
    panic!("the width generation never held still");
}

#[test]
fn memo_equality_is_identity() {
    let a = memo(text("x"));
    let b = memo(text("x"));
    assert_eq!(a, a.clone(), "a clone shares the Arc, so the memo");
    assert_ne!(a, b, "the same content in another memo is another node");
}

#[test]
fn raw_lines_are_moved_out_whole_at_any_width() {
    let lines = vec![StyledLine::text("abcdef", Style::DEFAULT)];
    assert_eq!(Layout::into_lines(Element::Raw(lines.clone()), 2), lines);
    assert_eq!(Layout::lay(&Element::Raw(lines.clone()), 2), lines);
}

#[test]
fn a_caret_sits_on_the_first_trailing_no_break_space_else_after_the_row() {
    let caret = Span::new(" ", Style::CARET);
    let one = vec![caret.clone()];
    let ls = Layout::lay(&caret_at(text("ab\u{a0}\u{a0}\u{a0}"), one.clone()), 20);
    assert_eq!(
        ls[0].spans,
        vec![
            Span::new("ab", Style::DEFAULT),
            caret,
            Span::new("\u{a0}\u{a0}", Style::DEFAULT)
        ]
    );
    let ls = Layout::lay(&caret_at(text("ab"), one.clone()), 20);
    assert_eq!(ls[0].plain(), "ab ");
    let ls = Layout::lay(&caret_at(empty(), one.clone()), 20);
    assert_eq!(ls, vec![StyledLine::new(one)]);
}

#[test]
fn a_wide_caret_covers_as_many_no_break_spaces_as_it_is_wide() {
    let caret = vec![Span::plain("x"), Span::plain("y")];
    let ls = Layout::lay(&caret_at(text("ab\u{a0}\u{a0}\u{a0}"), caret.clone()), 20);
    assert_eq!(ls[0].plain(), "abxy\u{a0}");
    let ls = Layout::lay(&caret_at(text("ab\u{a0}"), caret), 20);
    assert_eq!(ls[0].plain(), "abxy");
}

#[test]
fn a_table_sizes_columns_to_content_and_rules_under_the_header() {
    let cell = |s: &'static str| vec![Span::plain(s)];
    let e = table(
        Vec::new(),
        Vec::new(),
        vec![ColumnAlign::Left, ColumnAlign::Right],
        vec![cell("a"), cell("bb")],
        vec![vec![cell("ccc"), cell("d")]],
        Style::DIM,
    );
    assert_eq!(lay_plain(&e, 40), vec!["a     bb", "────────", "ccc    d"]);
    // 放不下时格子按词折行。
    let e = table(
        Vec::new(),
        Vec::new(),
        vec![ColumnAlign::Left, ColumnAlign::Left],
        vec![cell("name"), cell("note")],
        vec![vec![cell("x"), cell("alpha beta gamma")]],
        Style::DIM,
    );
    assert_eq!(
        lay_plain(&e, 16),
        vec![
            "name   note     ",
            "────────────────",
            "x      alpha    ",
            "       beta     ",
            "       gamma    "
        ]
    );
}
