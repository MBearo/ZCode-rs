//! 视觉设计背后的布局节点（docs/puffin-tui.md §4.2）。
//! 移植自 packages/tui/tui/test/src/zcode/tui/app/DesignLayoutSuite.scala（feat/tui a2bf878b34）。
use crate::render::{Color, Span, Style, StyledLine};

use super::super::element::{
    Wrap, beside, code_panel, first_fit, measure, single_line, slab, spread, stack, text, wrap_text,
};
use super::super::layout::Layout;
use super::plain;

const PANEL: Color = Color::rgb(33, 34, 38);

fn sp(s: &'static str, style: Style) -> Span {
    Span::new(s, style)
}

#[test]
fn a_slab_is_a_full_width_fill_with_half_block_edges_in_the_panel_colour() {
    let ls = Layout::lay(&slab(text("hi"), PANEL), 6);
    assert_eq!(plain(&ls), vec!["▄▄▄▄▄▄", "hi    ", "▀▀▀▀▀▀"]);
    let mut top: Vec<Color> = ls[0].spans.iter().map(|s| s.style.fg_color()).collect();
    top.dedup();
    assert_eq!(
        top,
        vec![PANEL],
        "the top edge is drawn in the panel colour"
    );
    assert!(
        ls[1].spans.iter().all(|s| s.style.bg_color() == PANEL),
        "every cell of a body row sits on the panel"
    );
    assert_eq!(ls[1].width(), 6, "body rows are padded to the full width");
}

#[test]
fn a_slab_keeps_its_contents_own_colours_and_only_fills_the_background() {
    let ls = Layout::lay(
        &slab(stack([text("a").style(Style::fg(Color::RED))]), PANEL),
        3,
    );
    assert_eq!(ls[1].spans[0].style.fg_color(), Color::RED);
    assert_eq!(ls[1].spans[0].style.bg_color(), PANEL);
}

#[test]
fn a_code_panel_prefix_edges_one_cell_of_padding_the_label_at_the_right_of_the_first_row() {
    let label = vec![sp("scala", Style::DIM)];
    let lines = ["val x = 1", "x"]
        .iter()
        .map(|l| vec![sp(l, Style::DEFAULT)])
        .collect();
    let ls = Layout::lay(
        &code_panel(vec![sp("   ", Style::DEFAULT)], lines, PANEL, label, false),
        20,
    );
    assert_eq!(
        plain(&ls),
        vec![
            "   ▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄".to_string(),
            "    val x = 1 scala ".to_string(),
            format!("    x{}", " ".repeat(15)),
            "   ▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀".to_string(),
        ]
    );
    assert!(
        ls[1].spans[1..].iter().all(|s| s.style.bg_color() == PANEL),
        "the prefix stays on the terminal background, the rest on the panel"
    );
}

#[test]
fn a_code_panel_drops_the_label_when_the_first_row_has_no_room_and_wraps_code_by_character() {
    let ls = Layout::lay(
        &code_panel(
            Vec::new(),
            vec![vec![sp("abcdefghij", Style::DEFAULT)]],
            PANEL,
            vec![sp("scala", Style::DIM)],
            false,
        ),
        8,
    );
    assert_eq!(plain(&ls)[1..3], [" abcdef ", " ghij   "]);
}

#[test]
fn a_code_panel_keeps_the_colours_of_its_code_and_puts_them_on_the_panel() {
    let ls = Layout::lay(
        &code_panel(
            Vec::new(),
            vec![vec![
                sp("val", Style::fg(Color::RED)),
                sp(" x", Style::DEFAULT),
            ]],
            PANEL,
            Vec::new(),
            false,
        ),
        12,
    );
    let kw = ls[1].spans.iter().find(|s| s.text == "val");
    assert_eq!(kw.map(|s| s.style), Some(Style::new(Color::RED, PANEL, 0)));
}

#[test]
fn measure_caps_the_layout_width_keeping_a_right_margin() {
    let e = measure(wrap_text("", "", "aa bb cc dd ee", Wrap::Word), 5, 2);
    assert_eq!(plain(&Layout::lay(&e, 40)), vec!["aa bb", "cc dd", "ee"]);
    assert_eq!(
        plain(&Layout::lay(&e, 6)),
        vec!["aa", "bb", "cc", "dd", "ee"],
        "a narrow terminal still keeps the margin"
    );
}

#[test]
fn spread_puts_left_and_right_on_one_row_first_fit_picks_the_first_variant_that_fits() {
    let left = vec![sp("● ready", Style::DEFAULT)];
    let right = vec![sp("^Q quit", Style::DEFAULT)];
    assert_eq!(
        plain(&Layout::lay(&spread(left.clone(), right.clone()), 20)),
        vec!["● ready      ^Q quit"]
    );
    let with = |extra: &'static str| {
        let mut l = left.clone();
        l.push(sp(extra, Style::DEFAULT));
        l
    };
    let variants = first_fit([
        spread(with("  model  ~/a/b"), right.clone()),
        spread(with("  model"), right.clone()),
        spread(left.clone(), right),
    ]);
    let first = |w: usize| Layout::lay(&variants, w)[0].plain();
    assert_eq!(
        first(40),
        format!("● ready  model  ~/a/b{}^Q quit", " ".repeat(12))
    );
    assert_eq!(first(24), format!("● ready  model{}^Q quit", " ".repeat(3)));
    assert_eq!(
        first(16),
        "● ready  ^Q quit",
        "a fit keeps at least two spaces between the sides"
    );
    assert_eq!(
        Layout::lay(&variants, 10)
            .iter()
            .map(StyledLine::width)
            .collect::<Vec<_>>(),
        vec![10],
        "nothing fits: the last variant, cut to the width"
    );
}

#[test]
fn a_single_line_is_cut_with_an_ellipsis_that_keeps_the_cut_spans_style() {
    let e = single_line(
        vec![
            sp("✓ ", Style::DEFAULT),
            sp("Read  packages/tui/x.scala", Style::DIM),
        ],
        false,
    );
    let ls = Layout::lay(&e, 12);
    assert_eq!(plain(&ls), vec!["✓ Read  pac…"]);
    assert_eq!(ls[0].spans.last().map(|s| s.style), Some(Style::DIM));
    assert_eq!(
        plain(&Layout::lay(&e, 40)),
        vec!["✓ Read  packages/tui/x.scala"]
    );
}

#[test]
fn a_single_line_can_keep_its_end_instead_for_paths() {
    let ls = Layout::lay(
        &single_line(vec![sp("~/a/b/c/project", Style::DIM)], true),
        10,
    );
    assert_eq!(plain(&ls), vec!["…c/project"]);
    assert_eq!(ls[0].spans[0].style, Style::DIM);
}

#[test]
fn beside_lays_two_blocks_side_by_side() {
    let e = beside(
        stack([text("x"), text("y")]),
        3,
        stack([text("a"), text("b"), text("c")]),
    );
    assert_eq!(plain(&Layout::lay(&e, 10)), vec!["x  a", "y  b", "   c"]);
}
