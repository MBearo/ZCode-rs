//! 槽的排版（docs/puffin-tui.md §4.2「按键提示（槽）」）：右沿、去掉明显的键、键的段、放到一行的右沿。
//! 移植自 packages/tui/tui/test/src/zcode/tui/shell/KeyHintsSuite.scala（feat/tui a2bf878b34）；随 `key_hints.rs` 从壳的
//! key_hints_tests.rs 搬来（docs/specs/rust-cli/tui-kit.md §4.4）。按键表（Puffin 的列）的用例留在壳里。
use crate::app::element::{Element, single_line};
use crate::app::layout::Layout;
use crate::app::widgets::key_hints::{self, edge, on_row, pick};
use crate::render::line::StyledLine;
use crate::render::{Span, Style};
use crate::theme::rgb::Rgb;
use crate::theme::theme::Theme;

fn rich() -> Theme {
    Theme::from(Rgb::new(214, 216, 220), Rgb::new(21, 22, 26))
}

fn lay(el: &Element, w: usize) -> Vec<String> {
    Layout::lay(el, w).iter().map(StyledLine::plain).collect()
}

fn slot(text: &str) -> Vec<Span> {
    vec![Span::new(text.to_owned(), Style::DEFAULT)]
}

fn pairs(ks: &[(&str, &str)]) -> Vec<(String, String)> {
    ks.iter()
        .map(|(k, l)| ((*k).to_owned(), (*l).to_owned()))
        .collect()
}

fn row(text: &str) -> Element {
    single_line(vec![Span::new(text.to_owned(), Style::DEFAULT)], false)
}

#[test]
fn the_slot_s_edge_is_the_input_box_s_width_minus_1_one_blank_cell_to_the_right() {
    assert_eq!(edge(80), 79);
    assert_eq!(edge(100), 99);
    assert_eq!(edge(160), 159, "宽终端上它在框的右端，不跟正文");
    assert_eq!(edge(1), 0);
    assert_eq!(edge(0), 0);
}

#[test]
fn the_slot_drops_the_obvious_keys_and_keeps_at_most_two() {
    assert_eq!(
        pick(&pairs(&[
            ("↑↓", "choose"),
            ("⏎", "insert"),
            ("esc", "close")
        ])),
        pairs(&[("⏎", "insert")])
    );
    assert!(
        pick(&pairs(&[
            ("↑↓", "choose"),
            ("⏎", "send"),
            ("esc", "close"),
            ("^Q", "quit")
        ]))
        .is_empty()
    );
    assert_eq!(
        pick(&pairs(&[
            ("←→", "run"),
            ("⏎", "peek"),
            ("^X", "stop"),
            ("esc", "close")
        ])),
        pairs(&[("←→", "run"), ("⏎", "peek")])
    );
    // esc 只有「关闭」是显而易见的；停下与返回要写出来。
    assert_eq!(pick(&pairs(&[("esc", "back")])), pairs(&[("esc", "back")]));
    assert_eq!(
        pick(&pairs(&[("⏎", "try again"), ("esc", "back")])),
        pairs(&[("⏎", "try again"), ("esc", "back")])
    );
}

#[test]
fn keys_are_fg2_their_words_fg3_two_spaces_between_pairs() {
    let t = rich();
    let spans = key_hints::spans(&t, &pairs(&[("⏎", "peek"), ("^X", "stop")]));
    assert_eq!(
        spans.iter().map(|s| s.text.as_ref()).collect::<String>(),
        "⏎ peek  ^X stop"
    );
    assert_eq!(
        spans.iter().find(|s| s.text == "⏎").map(|s| s.style),
        Some(t.fg2)
    );
    assert_eq!(
        spans.iter().find(|s| s.text == " peek").map(|s| s.style),
        Some(t.fg3)
    );
}

#[test]
fn the_slot_ends_at_the_edge_when_the_row_leaves_two_cells_else_the_row_is_drawn_alone() {
    let hello = row(" › hello");
    assert_eq!(
        lay(&on_row(hello.clone(), slot("⇧⏎ newline")), 80),
        [format!(" › hello{}⇧⏎ newline", " ".repeat(61))]
    );
    // 正好空两格：画；只空一格：让开。
    let near = row(&format!(" › {}", "x".repeat(64)));
    assert_eq!(
        lay(&on_row(near, slot("⇧⏎ newline")), 80)[0]
            .chars()
            .count(),
        79
    );
    let close = row(&format!(" › {}", "x".repeat(65)));
    assert_eq!(
        lay(&on_row(close, slot("⇧⏎ newline")), 80),
        [format!(" › {}", "x".repeat(65))]
    );
    // 折成两行的不放槽（草稿的槽由 draft_rows 放在最后一行）。
    let wraps = row(&format!(" › {}", "y".repeat(100)));
    assert_eq!(
        lay(&on_row(wraps.clone(), slot("esc stop")), 80),
        lay(&wraps, 80)
    );
    assert_eq!(lay(&on_row(hello.clone(), Vec::new()), 80), lay(&hello, 80));
}
