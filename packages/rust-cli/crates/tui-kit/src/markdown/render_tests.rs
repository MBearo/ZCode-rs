//! MarkdownRenderSuite 与 AnswerRenderCacheSuite 的移植。
//! 移植自 packages/tui/tui/test/src/zcode/tui/markdown/MarkdownRenderSuite.scala 与 AnswerRenderCacheSuite.scala
//! （feat/tui a2bf878b34）。这个文件是 plain 主题下的块与行内；流式、rich 与缓存在子模块里。
use crate::app::element::Element;
use crate::app::layout::Layout;
use crate::markdown::document::Document;
use crate::markdown::render::render;
use crate::render::{Color, StyledLine, attr, width};
use crate::theme::Rgb;
use crate::theme::theme::Theme;

mod cache;
mod rich;
mod streaming;

pub(super) fn doc(s: &str) -> Document {
    Document::parse(s)
}

pub(super) fn laid(e: &Element, w: usize) -> Vec<StyledLine> {
    Layout::lay(e, w)
}

pub(super) fn lines_at(e: &Element, w: usize) -> Vec<String> {
    Layout::lay(e, w).iter().map(StyledLine::plain).collect()
}

pub(super) fn lines(e: &Element) -> Vec<String> {
    lines_at(e, 60)
}

fn sgr(e: &Element) -> String {
    Layout::lay(e, 60)
        .iter()
        .map(StyledLine::render)
        .collect::<Vec<_>>()
        .join("\n")
}

/// plain 主题下渲染一篇完整的文档。
pub(super) fn md(s: &str) -> Element {
    render(&doc(s), &Theme::PLAIN)
}

/// Scala 套件的 `Rich`。
pub(super) fn rich_theme() -> Theme {
    Theme::from(Rgb::new(214, 216, 220), Rgb::new(21, 22, 26))
}

pub(super) fn v(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn a_paragraph_starts_at_column_3() {
    assert_eq!(lines(&md("hello world")), v(&["   hello world"]));
}

#[test]
fn emphasis_and_strong_strip_to_plain_text_but_carry_sgr_attributes() {
    assert_eq!(lines(&md("*hi*")), v(&["   hi"]));
    assert!(sgr(&md("*hi*")).contains(";3"), "italic SGR expected");
    assert_eq!(lines(&md("**hi**")), v(&["   hi"]));
    assert!(sgr(&md("**hi**")).contains(";1"), "bold SGR expected");
}

#[test]
fn a_level_1_heading_has_no_rule_under_it() {
    assert_eq!(
        lines(&md("# Title\n\nbody")),
        v(&["   Title", "", "   body"])
    );
}

#[test]
fn bullet_and_ordered_list_markers() {
    assert_eq!(lines(&md("- a\n- b")), v(&["   • a", "   • b"]));
    assert_eq!(lines(&md("1. a\n2. b")), v(&["   1. a", "   2. b"]));
}

#[test]
fn bullets_change_with_depth_then_wrap_around() {
    assert_eq!(
        lines(&md("- a\n  - b\n    - c\n      - d")),
        v(&["   • a", "     ◦ b", "       ▪ c", "         • d"])
    );
}

#[test]
fn ordinals_right_align_to_the_widest_one_and_a_wrapped_item_hangs_past_it() {
    assert_eq!(
        lines(&md("8. a\n9. b\n10. c")),
        v(&["    8. a", "    9. b", "   10. c"])
    );
    let ls = lines_at(&md("9. x\n10. alpha beta gamma"), 14);
    assert_eq!(
        ls,
        v(&["    9. x", "   10. alpha", "       beta", "       gamma"])
    );
}

#[test]
fn a_tasks_mark_replaces_the_bullet() {
    assert_eq!(
        lines(&md("- [x] done\n- [ ] todo")),
        v(&["   ✓ done", "   □ todo"])
    );
}

#[test]
fn a_wrapped_list_item_hangs_under_its_first_character() {
    // 窄宽度让条目折行；接续行对齐到标记之后。
    let ls = lines_at(&md("- alpha beta gamma delta"), 12);
    assert!(ls.len() > 1, "{ls:?}");
    assert!(ls[0].starts_with("   • "), "{}", ls[0]);
    // 接续行挂在第一个内容字符下（"• " 之后）。
    assert!(ls[1..].iter().all(|l| l.starts_with("     ")), "{ls:?}");
}

#[test]
fn a_fenced_code_block_draws_a_dim_left_rail() {
    assert_eq!(lines(&md("```\nval x = 1\n```")), v(&["   ▏ val x = 1"]));
}

#[test]
fn a_fenced_code_block_shows_its_language_tag() {
    assert_eq!(lines(&md("```scala\nx\n```")), v(&["   scala", "   ▏ x"]));
}

#[test]
fn a_blockquote_draws_a_thin_rail() {
    assert_eq!(lines(&md("> quoted")), v(&["   │ quoted"]));
}

#[test]
fn strikethrough_is_sgr_9_with_no_combining_marks() {
    assert_eq!(lines(&md("~~gone~~")), v(&["   gone"]));
    let s = sgr(&md("~~gone~~"));
    assert!(s.contains(";9"), "{s}");
}

#[test]
fn plain_inline_code_keeps_its_backticks_dim() {
    let ls = laid(&md("see `x`"), 60);
    assert_eq!(
        ls.iter().map(StyledLine::plain).collect::<Vec<_>>(),
        v(&["   see `x`"])
    );
    let spans = &ls[0].spans;
    let ticks: Vec<_> = spans.iter().filter(|sp| sp.text.contains('`')).collect();
    assert!(
        !ticks.is_empty() && ticks.iter().all(|sp| sp.style.has_attr(attr::DIM)),
        "{spans:?}"
    );
    assert!(
        spans.iter().any(|sp| sp.text == "x"
            && sp.style.fg_color() == Color::Default
            && !sp.style.has_attr(attr::DIM)),
        "{spans:?}"
    );
}

#[test]
fn inline_code_stays_whole_when_it_fits_and_breaks_by_character_when_wider() {
    assert_eq!(
        lines_at(&md("see `pnpm test` now"), 14),
        v(&["   see", "   `pnpm test`", "   now"])
    );
    let long = lines_at(&md("`aaaa bbbb cccc`"), 10);
    assert!(long.iter().all(|l| l.chars().count() <= 10), "{long:?}");
    let joined: String = long
        .iter()
        .map(|l| l.chars().skip(3).collect::<String>())
        .collect();
    assert_eq!(joined, "`aaaa bbbb cccc`");
}

#[test]
fn a_code_span_in_a_table_cell_keeps_the_column_wide_enough_to_stay_whole() {
    // 宽 16：两列要分 10 格，单元格得折行；代码那一列至少留出整段代码的宽度。
    let ls = lines_at(&md("| a | b |\n|---|---|\n| `x y` | bbbb bbbb bbbb |"), 16);
    assert!(ls.iter().any(|l| l.contains("`x y`")), "{}", ls.join("|"));
}

#[test]
fn a_picture_on_its_own_is_image_and_its_alt_text_inside_a_sentence_just_the_alt() {
    assert_eq!(
        lines(&md("![the update screen](docs/update.png)")),
        v(&["   Image  the update screen"])
    );
    assert_eq!(
        lines(&md("see ![alt](x.png) here")),
        v(&["   see alt here"])
    );
}

#[test]
fn a_thematic_break_is_a_hairline_from_the_content_indent() {
    let ls = lines(&md("---"));
    assert_eq!(ls.len(), 1);
    assert!(
        ls[0].starts_with("   ─") && ls[0].chars().skip(3).all(|c| c == '─'),
        "{}",
        ls[0]
    );
}

#[test]
fn a_table_has_no_vertical_lines() {
    assert_eq!(
        lines(&md("| a | b |\n|---|---|\n| 1 | 2 |")),
        v(&["   a   b", "   ─────", "   1   2"])
    );
}

#[test]
fn inline_code_in_a_cell_counts_its_backticks_in_the_column_width() {
    assert_eq!(
        lines(&md("| `ab` | c |\n|---|---|\n| 1 | 2 |")),
        v(&["   `ab`   c", "   ────────", "   1      2"])
    );
}

#[test]
fn a_table_that_fits_keeps_natural_column_widths() {
    // 宽终端：每个逻辑行一个物理行（表头、横线、一行内容）。
    assert_eq!(
        lines_at(&md("| a | b |\n|---|---|\n| 1 | 2 |"), 80).len(),
        3
    );
}

#[test]
fn an_overlong_cell_wraps_within_the_screen_width() {
    let src = "| name | description |\n|---|---|\n| x | a very long description that has to wrap across several lines |";
    let ls = lines_at(&md(src), 30);
    assert!(
        ls.iter().all(|l| width::string_width(l) <= 30),
        "{}",
        ls.join("\n")
    );
    // 格子折行后，内容行跨好几个物理行。
    assert!(ls.len() > 3, "{}", ls.join("\n"));
}

#[test]
fn column_alignment_pads_cells() {
    // 第 0 列右对齐，表头 "long" 把列宽定为 4。
    assert_eq!(
        lines(&md("| long | b |\n|---:|:--|\n| 1 | 2 |")),
        v(&["   long   b", "   ────────", "      1   2"])
    );
}

#[test]
fn paragraphs_are_separated_by_a_blank_line() {
    assert_eq!(lines(&md("one\n\ntwo")), v(&["   one", "", "   two"]));
}
