//! 页的复制（page_copy.rs）：复制出的字。
use rust_cli_tui_kit::render::{Span, Style, StyledLine};

use super::*;

fn line(text: &'static str) -> StyledLine {
    StyledLine::new(vec![Span::new(text, Style::DEFAULT)])
}

#[test]
fn the_text_drops_the_blank_edges_trailing_space_and_the_common_indent() {
    let page = [
        StyledLine::EMPTY,
        line("   Stats  ›  Context   "),
        StyledLine::EMPTY,
        line("   Categories"),
        line("     System prompt  1.2k"),
        StyledLine::EMPTY,
    ];
    assert_eq!(
        text_of(&page),
        "Stats  ›  Context\n\nCategories\n  System prompt  1.2k"
    );
    assert_eq!(text_of(&[StyledLine::EMPTY]), "");
}
