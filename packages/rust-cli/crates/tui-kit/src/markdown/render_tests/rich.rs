//! rich 主题下方向 A 的墨色（docs/puffin-tui.md §4.2「回答的 Markdown」）：代码面板、标题、行内代码、链接、引用、
//! 任务、表格、图片与语法墨。
//! 移植自 packages/tui/tui/test/src/zcode/tui/markdown/MarkdownRenderSuite.scala（feat/tui a2bf878b34）。
use crate::markdown::render::render;
use crate::render::{Color, Span, Style, StyledLine, attr};
use crate::theme::theme::Theme;

use super::{doc, laid, lines, md, rich_theme, v};

fn rich_at(src: &str, w: usize) -> Vec<StyledLine> {
    laid(&render(&doc(src), &rich_theme()), w)
}

fn rich(src: &str) -> Vec<StyledLine> {
    rich_at(src, 60)
}

fn span_of(ls: &[StyledLine], text: &str) -> Span {
    ls.iter()
        .flat_map(|l| l.spans.iter())
        .find(|sp| sp.text == text)
        .cloned()
        .unwrap_or_else(|| panic!("no span '{text}' in {ls:?}"))
}

#[test]
fn rich_code_sits_on_a_panel_with_the_language_at_the_right() {
    let ls = rich_at("```scala\nval x = 1\n```", 30);
    let plain: Vec<String> = ls.iter().map(StyledLine::plain).collect();
    assert_eq!(
        plain,
        vec![
            format!("   {}", "▄".repeat(27)),
            format!("    val x = 1{}scala ", " ".repeat(11)),
            format!("   {}", "▀".repeat(27)),
        ]
    );
    assert_eq!(
        ls[1].spans.last().unwrap().style.bg_color(),
        rich_theme().panel.unwrap()
    );
}

#[test]
fn rich_a_code_panels_edges_replace_the_blank_rows_around_it() {
    let ls = rich_at("before\n\n```\nx\n```\n\nafter", 20);
    let firsts: Vec<String> = ls
        .iter()
        .map(|l| l.plain().trim().chars().take(1).collect())
        .collect();
    assert_eq!(firsts, v(&["b", "▄", "x", "▀", "a"]));
}

#[test]
fn plain_blocks_around_code_keep_their_blank_rows() {
    assert_eq!(
        lines(&md("before\n\n```\nx\n```\n\nafter")),
        v(&["   before", "", "   ▏ x", "", "   after"])
    );
}

#[test]
fn rich_headings_inline_code_links_and_bullets_take_their_tokens() {
    let r = rich_theme();
    let first = |src: &str| {
        rich(src)[0]
            .spans
            .iter()
            .find(|sp| !sp.text.trim().is_empty())
            .unwrap()
            .style
    };
    assert_eq!(first("# A").fg_color(), r.brand);
    assert_eq!(first("## A").fg_color(), Color::Default);
    assert_eq!(first("### A").fg_color(), r.fg2.fg_color());
    assert_eq!(
        first("`x`").fg_color(),
        r.tick.fg_color(),
        "the opening backtick"
    );
    assert_eq!(first("[x](https://example.com)").fg_color(), r.brand);
    assert_eq!(
        first("- a").fg_color(),
        r.fg3.fg_color(),
        "bullets are quiet"
    );
}

#[test]
fn rich_inline_code_is_cool_grey_between_half_tone_backticks() {
    let r = rich_theme();
    let ls = rich("see `x` now");
    assert_eq!(ls[0].plain(), "   see `x` now");
    assert_eq!(ls.len(), 1);
    let spans = &ls[0].spans;
    let i = spans.iter().position(|sp| sp.text == "x").unwrap();
    assert_eq!(spans[i].style.fg_color(), r.code_ink.fg_color());
    assert_eq!(spans[i - 1].style.fg_color(), r.tick.fg_color());
    assert_eq!(spans[i + 1].style.fg_color(), r.tick.fg_color());
    assert_eq!((&*spans[i - 1].text, &*spans[i + 1].text), ("`", "`"));
}

#[test]
fn rich_inline_code_in_bold_or_a_heading_takes_the_weight_not_the_colour() {
    let r = rich_theme();
    let bold = rich("**a `x` b**");
    assert!(
        bold[0]
            .spans
            .iter()
            .filter(|sp| !sp.text.trim().is_empty())
            .all(|sp| sp.style.has_attr(attr::BOLD)),
        "{:?}",
        bold[0].spans
    );
    assert_eq!(span_of(&bold, "x").style.fg_color(), r.code_ink.fg_color());
    let h1 = rich("# Why `x`");
    assert_eq!(
        span_of(&h1, "x").style,
        r.code_ink.with_attr(attr::BOLD | attr::WHOLE)
    );
    assert_eq!(
        span_of(&h1, "`").style,
        r.tick.with_attr(attr::BOLD | attr::WHOLE)
    );
}

#[test]
fn rich_inside_a_link_the_link_wins_backticks_too() {
    let r = rich_theme();
    let ls = rich("[`x`](https://example.com)");
    let spans: Vec<&Span> = ls[0]
        .spans
        .iter()
        .filter(|sp| !sp.text.trim().is_empty())
        .collect();
    assert_eq!(spans.iter().map(|sp| &*sp.text).collect::<String>(), "`x`");
    assert!(
        spans
            .iter()
            .all(|sp| sp.style.fg_color() == r.brand && sp.style.has_attr(attr::UNDERLINE)),
        "{spans:?}"
    );
}

/// brand 向背景混合 45 %。
const UNDERLINE_INK: Color = Color::TrueColor(0x37, 0x72, 0x95);

#[test]
fn rich_a_link_is_brand_underlined_in_a_quieter_brand_and_opens_its_target() {
    let sp = span_of(
        &rich("see [the docs](https://example.com/a) now"),
        "the docs",
    );
    assert_eq!(sp.style.fg_color(), rich_theme().brand);
    assert!(sp.style.has_attr(attr::UNDERLINE));
    assert_eq!(sp.decor.underline, UNDERLINE_INK);
    assert_eq!(sp.decor.link, "https://example.com/a");
}

#[test]
fn a_target_without_a_scheme_only_looks_like_a_link_autolinks_open_email_through_mailto() {
    assert_eq!(
        span_of(&rich("[notes](docs/notes.md)"), "notes").decor.link,
        ""
    );
    assert_eq!(
        span_of(&rich("<https://example.com>"), "https://example.com")
            .decor
            .link,
        "https://example.com"
    );
    assert_eq!(
        span_of(&rich("<dev@example.com>"), "dev@example.com")
            .decor
            .link,
        "mailto:dev@example.com"
    );
    // plain：照样能点，下划线不另设颜色。
    let plain = span_of(&laid(&md("[x](https://example.com)"), 60), "x");
    assert_eq!(plain.decor.link, "https://example.com");
    assert_eq!(plain.decor.underline, Color::Default);
}

#[test]
fn rich_strikethrough_is_fg3() {
    let sp = span_of(&rich("~~gone~~"), "gone");
    assert!(sp.style.has_attr(attr::STRIKE));
    assert_eq!(sp.style.fg_color(), rich_theme().fg3.fg_color());
}

#[test]
fn rich_a_quotes_rail_is_the_line_colour_and_its_text_fg2() {
    let r = rich_theme();
    let ls = rich("> quoted");
    assert_eq!(span_of(&ls, "│").style, r.line);
    assert_eq!(span_of(&ls, "quoted").style.fg_color(), r.fg2.fg_color());
}

#[test]
fn rich_a_done_task_is_a_tick_in_ok_with_fg3_text_an_open_one_a_box_in_fg3() {
    let r = rich_theme();
    let ls = rich("- [x] done\n- [ ] todo");
    assert_eq!(span_of(&ls, "✓").style.fg_color(), r.ok);
    assert_eq!(span_of(&ls, "done").style.fg_color(), r.fg3.fg_color());
    assert_eq!(span_of(&ls, "□").style.fg_color(), r.fg3.fg_color());
    assert_eq!(span_of(&ls, "todo").style.fg_color(), Color::Default);
}

#[test]
fn rich_a_tables_header_is_bold_and_its_rule_the_line_colour() {
    let ls = rich("| a | b |\n|---|---|\n| 1 | 2 |");
    assert!(span_of(&ls, "a").style.has_attr(attr::BOLD));
    assert_eq!(span_of(&ls, "─────").style, rich_theme().line);
}

#[test]
fn rich_a_picture_is_fg3_image_and_its_alt_text_in_fg3_italic() {
    let r = rich_theme();
    let ls = rich("![alt](x.png)");
    assert_eq!(span_of(&ls, "Image").style, r.fg3);
    assert_eq!(span_of(&ls, "alt").style, r.fg3.with_attr(attr::ITALIC));
}

#[test]
fn rich_code_has_the_themes_syntax_inks_on_the_panel() {
    let r = rich_theme();
    let ls = rich("```scala\nval x: Seq = f(\"s\" + 1, true) // one\n```");
    let panel = r.panel.unwrap();
    let ink = |t: &str| span_of(&ls, t).style;
    let y = r.syntax;
    assert_eq!(ink("val").fg_color(), y.keyword);
    assert_eq!(ink("Seq").fg_color(), y.ty);
    assert_eq!(ink("f").fg_color(), y.function);
    assert_eq!(ink("\"s\"").fg_color(), y.string);
    assert_eq!(ink("1").fg_color(), y.number);
    assert_eq!(ink("true").fg_color(), y.constant);
    assert_eq!(ink("// one"), r.fg3.with_attr(attr::ITALIC).with_bg(panel));
    assert!(
        ["val", "\"s\"", "1"]
            .iter()
            .all(|t| ink(t).bg_color() == panel)
    );
    let json = rich("```json\n{\"k\": 1}\n```");
    assert_eq!(span_of(&json, "\"k\"").style.fg_color(), y.key);
}

#[test]
fn code_in_a_language_we_do_not_know_and_plain_mode_have_no_syntax_colours() {
    let unknown = rich("```cobol\nMOVE 1 TO X\n```");
    assert!(
        unknown[1]
            .spans
            .iter()
            .all(|sp| sp.style.fg_color() == Color::Default || sp.text.trim() == "cobol"),
        "{:?}",
        unknown[1].spans
    );
    let plain = laid(&render(&doc("```scala\nval x = 1\n```"), &Theme::PLAIN), 60);
    let spans: Vec<&Span> = plain
        .iter()
        .flat_map(|l| l.spans.iter())
        .filter(|sp| !sp.text.contains('▏') && sp.text != "scala")
        .collect();
    assert!(
        spans.iter().all(|sp| sp.style == Style::DEFAULT),
        "{spans:?}"
    );
}
