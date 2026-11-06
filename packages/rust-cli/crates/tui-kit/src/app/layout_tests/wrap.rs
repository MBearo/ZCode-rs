//! 经公开的 `wrap_text` 元素与布局测共用的折行引擎。
//! 移植自 packages/tui/tui/test/src/zcode/tui/app/WrapSuite.scala（feat/tui a2bf878b34）。
use crate::render::{Color, Continuation, Span, Style, ansi, attr, width};

use super::super::element::{
    Element, Wrap, beside, code_panel, round_box, slab, text, wrap_styled, wrap_text,
};
use super::super::layout::Layout;

fn lines(e: &Element, w: usize) -> Vec<String> {
    super::lay_plain(e, w)
}

fn conts(e: &Element, w: usize) -> Vec<Option<Continuation>> {
    Layout::lay(e, w).iter().map(|l| l.cont).collect()
}

fn word(s: &'static str) -> Element {
    wrap_text("", "", s, Wrap::Word)
}

fn cont(at: usize, space: bool) -> Option<Continuation> {
    Some(Continuation { at, space })
}

#[test]
fn word_mode_breaks_between_words_not_mid_word() {
    assert_eq!(
        lines(&word("alpha beta gamma"), 9),
        vec!["alpha", "beta", "gamma"]
    );
}

#[test]
fn word_mode_packs_as_many_words_as_fit_per_line() {
    // "aa bb" = 5 ≤ 6；再加 " cc" 是 8 > 6。
    assert_eq!(lines(&word("aa bb cc dd"), 6), vec!["aa bb", "cc dd"]);
}

#[test]
fn a_word_longer_than_the_line_is_character_broken_as_a_last_resort() {
    assert_eq!(lines(&word("abcdefgh"), 4), vec!["abcd", "efgh"]);
}

#[test]
fn word_mode_breaks_between_wide_characters() {
    // 修复前：没有空格的一段中文算一个词，放不下就整段换到下一行，第一行留空；流式输出时它先写在第一行，长过行尾时
    // 整段跳到下一行（§4.2「流式节奏」的「词不移动」）。
    assert_eq!(
        lines(&word("ab 页脚换行是因为"), 10),
        vec!["ab 页脚换", "行是因为"]
    );
    assert_eq!(lines(&word("按UTF-16码元"), 8), vec!["按UTF-16", "码元"]);
    // 宽字符之间的折行不丢空格。
    assert_eq!(
        conts(&word("页脚换行是因为"), 6),
        vec![None, cont(0, false), cont(0, false)]
    );
}

#[test]
fn an_explicit_newline_always_breaks() {
    assert_eq!(lines(&word("a\nb"), 40), vec!["a", "b"]);
}

#[test]
fn continuation_lines_use_the_next_prefix() {
    let ls = lines(&wrap_text("> ", "| ", "alpha beta", Wrap::Word), 7);
    assert_eq!(ls[0], "> alpha");
    assert!(ls[1..].iter().all(|l| l.starts_with("| ")), "{ls:?}");
}

#[test]
fn char_mode_breaks_at_any_code_point() {
    assert_eq!(
        lines(&wrap_text("", "", "abcdef", Wrap::Char), 4),
        vec!["abcd", "ef"]
    );
}

#[test]
fn a_trailing_styled_space_is_preserved() {
    // 末尾的反显空格（输入框的光标格）折行时必须留下。
    let value = format!("hi{} {}", Style::REVERSE.set_sequence(), ansi::RESET);
    assert_eq!(
        lines(&wrap_text("", "", value, Wrap::Word), 40),
        vec!["hi "]
    );
}

#[test]
fn a_styled_space_before_a_hard_newline_survives() {
    // 下划线的光标格停在一行末尾、后面还有行：上下移动光标夹到较短的行时就是这样。
    let value = format!("ab{} {}\ncd", Style::UNDERLINE.set_sequence(), ansi::RESET);
    let laid = Layout::lay(&wrap_text("", "", value, Wrap::Word), 40);
    assert_eq!(super::plain(&laid), vec!["ab ", "cd"]);
    // 末尾的格子保留下划线（硬换行处不折叠）。
    assert!(
        laid[0]
            .spans
            .iter()
            .any(|s| s.text == " " && s.style.has_attr(attr::UNDERLINE)),
        "{:?}",
        laid[0].spans
    );
}

#[test]
fn a_plain_trailing_space_before_a_hard_newline_is_still_collapsed() {
    assert_eq!(lines(&word("ab \ncd"), 40), vec!["ab", "cd"]);
}

#[test]
fn wrapped_lines_never_exceed_the_width() {
    let t = "the quick brown fox jumps over the lazy dog";
    for w in [8, 12, 20] {
        assert!(
            lines(&word(t), w)
                .iter()
                .all(|l| width::string_width(l) <= w),
            "width {w}"
        );
    }
}

/* ---- 软折行有标记，复制时能接起来（docs/puffin-tui.md §4.4） ---- */

#[test]
fn a_soft_wrap_marks_the_row_after_it() {
    assert_eq!(
        conts(&wrap_text("> ", "| ", "alpha beta", Wrap::Word), 7),
        vec![None, cont(2, true)]
    );
}

#[test]
fn an_explicit_newline_is_a_hard_break_not_a_soft_wrap() {
    assert_eq!(conts(&word("a\nb"), 40), vec![None, None]);
}

#[test]
fn a_long_word_broken_mid_word_drops_no_space() {
    assert_eq!(conts(&word("abcdefgh"), 4), vec![None, cont(0, false)]);
}

#[test]
fn char_mode_marks_its_breaks_too() {
    assert_eq!(
        conts(&wrap_text("", "", "abcdef", Wrap::Char), 4),
        vec![None, cont(0, false)]
    );
}

#[test]
fn wrappers_that_add_columns_in_front_move_the_mark_with_them() {
    assert_eq!(
        conts(&round_box(word("alpha beta"), Color::Default), 9),
        vec![None, None, cont(2, true), None]
    );
    assert_eq!(
        conts(&beside(text("●"), 2, word("alpha beta")), 7),
        vec![None, cont(2, true)]
    );
    assert_eq!(
        conts(&slab(word("alpha beta"), Color::Named(1)), 7),
        vec![None, None, cont(0, true), None]
    );
}

#[test]
fn a_code_panels_wrapped_line_continues_after_the_panels_padding() {
    let e = code_panel(
        vec![Span::plain("  ")],
        vec![vec![Span::plain("abcdefgh")]],
        Color::Named(1),
        Vec::new(),
        false,
    );
    assert_eq!(conts(&e, 9), vec![None, None, cont(3, false), None]);
}

#[test]
fn restyling_cells_keeps_the_mark() {
    let l = Layout::lay(&word("alpha beta"), 7).swap_remove(1);
    assert_eq!(
        l.restyle_cells(0, 2, |s| s.with_attr(attr::BOLD)).cont,
        l.cont
    );
}

#[test]
fn spans_merge_one_style_and_decor_across_source_spans_and_astral_code_points() {
    let bold = Style::BOLD;
    let src = |mid: &'static str| {
        vec![
            Span::plain("ab"),
            Span::plain(mid),
            Span::new("d", bold),
            Span::new("e", bold),
            Span::plain(" fg"),
        ]
    };
    let e = wrap_styled(Vec::new(), Vec::new(), src("c🙂"), Wrap::Word);
    assert_eq!(
        Layout::lay(&e, 40)
            .into_iter()
            .map(|l| l.spans)
            .collect::<Vec<_>>(),
        vec![vec![
            Span::plain("abc🙂"),
            Span::new("de", bold),
            Span::plain(" fg")
        ]]
    );
    // 在宽字符处折开：每行的段是自己的；折行之后的那一行开始新的段。
    let wide = wrap_styled(Vec::new(), Vec::new(), src("c中"), Wrap::Word);
    assert_eq!(
        Layout::lay(&wide, 6)
            .into_iter()
            .map(|l| l.spans)
            .collect::<Vec<_>>(),
        vec![
            vec![Span::plain("abc中")],
            vec![Span::new("de", bold), Span::plain(" fg")]
        ]
    );
}

#[test]
fn a_whole_styled_space_does_not_break_its_run() {
    // 行内代码的空格（Whole）不是折行处：整段放得下时一起移到下一行（docs/puffin-tui.md §4.2「回答的 Markdown」）。
    let code = Style::attrs_only(attr::WHOLE);
    let e = wrap_styled(
        Vec::new(),
        Vec::new(),
        vec![Span::plain("see "), Span::new("a b", code)],
        Wrap::Word,
    );
    assert_eq!(lines(&e, 6), vec!["see", "a b"]);
}
