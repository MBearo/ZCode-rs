// 复制自 apps/zcode-host/crates/tui/src/ui/markdown_tests.rs（feat/rs 79b6691c55）。
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use super::*;
use crate::ui::palette::{ColorLevel, Palette};
use crate::ui::text::plain;

fn lines(text: &str, width: usize) -> Vec<String> {
    let palette = Palette::new(ColorLevel::Ansi16, None);
    render(
        text,
        width,
        vec![Span::raw("• ")],
        vec![Span::raw("  ")],
        Style::default(),
        &palette,
    )
    .iter()
    .map(plain)
    .collect()
}

#[test]
fn paragraphs_hang_under_the_cell_prefix_and_are_separated() {
    assert_eq!(
        lines("first paragraph wraps here\n\nsecond", 16),
        ["• first ", "  paragraph ", "  wraps here", "  ", "  second"]
    );
}

#[test]
fn lists_nest_by_four_columns_and_task_markers_replace_bullets() {
    let text = "- [ ] parent task\n  - [x] nested\n  - child\n- done";
    assert_eq!(
        lines(text, 40),
        [
            "• ☐ parent task",
            "      ☑ nested",
            "      • child",
            "  • done"
        ]
    );
    assert_eq!(lines("1. one\n2. two", 40), ["• 1. one", "  2. two"]);
}

#[test]
fn quotes_code_blocks_and_tables_keep_their_structure() {
    assert_eq!(lines("> quoted", 40), ["• ▎ quoted"]);
    assert_eq!(lines("```\nlet x = 1;\n```", 40), ["• let x = 1;"]);
    // 表格用线框，表头下一条分隔线。
    assert_eq!(
        lines("| a | b |\n|---|---|\n| 1 | 22 |", 40),
        [
            "• ┌───┬────┐",
            "  │ a │ b  │",
            "  ├───┼────┤",
            "  │ 1 │ 22 │",
            "  └───┴────┘"
        ]
    );
    // 放不下的表格转成记录。
    assert_eq!(
        lines(
            "| name | description |\n|---|---|\n| x | a long description |",
            14
        ),
        [
            "• name: x",
            "  description:",
            "  a long ",
            "  description",
            "  "
        ]
    );
}

#[test]
fn an_unclosed_code_block_while_streaming_is_still_shown() {
    assert_eq!(
        lines("text\n\n```rust\nfn main()", 40),
        ["• text", "  ", "  fn main()"]
    );
}

#[test]
fn headings_are_bold_and_markers_are_hidden() {
    let palette = Palette::new(ColorLevel::Ansi16, None);
    let out = render(
        "# Title\n**bold** and `code`",
        40,
        vec![],
        vec![],
        Style::default(),
        &palette,
    );
    assert_eq!(plain(&out[0]), "Title");
    assert!(
        out[0].spans[0]
            .style
            .add_modifier
            .contains(Modifier::BOLD | Modifier::UNDERLINED)
    );
    assert_eq!(plain(&out[2]), "bold and code");
}

#[test]
fn web_links_show_their_target_once() {
    assert_eq!(
        lines(
            "see [docs](https://example.com/d) and https://example.com/x",
            80
        ),
        ["• see docs (https://example.com/d) and https://example.com/x"]
    );
    assert_eq!(lines("[a.rs](./src/a.rs)", 40), ["• a.rs"]);
}
