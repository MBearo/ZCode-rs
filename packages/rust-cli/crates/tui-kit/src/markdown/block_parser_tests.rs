//! 块解析的测试：BlockParserSuite、ListSuite、TableSuite。
//! 移植自 packages/tui/tui/test/src/zcode/tui/markdown/{BlockParserSuite,ListSuite,TableSuite}.scala（feat/tui a2bf878b34）。
use super::ast::{Align, Block, Inline, ListItem};
use super::block_parser::{parse, split_lines};
use super::inline_parser;

fn text(s: &str) -> Inline {
    Inline::Text(s.to_owned())
}

fn para(s: &str) -> Block {
    Block::Paragraph(inline_parser::parse(s))
}

fn heading(level: u8, content: Vec<Inline>) -> Block {
    Block::Heading { level, content }
}

fn code(info: Option<&str>, c: &str, fenced: bool) -> Block {
    Block::Code {
        info: info.map(str::to_owned),
        lang: info.map(str::to_owned),
        code: c.to_owned(),
        fenced,
    }
}

fn list(parsed: &[Block]) -> (bool, u32, bool, &[ListItem]) {
    match &parsed[0] {
        Block::List {
            ordered,
            start,
            tight,
            items,
        } => (*ordered, *start, *tight, items),
        other => panic!("expected a list, got {other:?}"),
    }
}

/// 表格的（对齐，表头，行）。
type TableParts<'a> = (&'a [Align], &'a [Vec<Inline>], &'a [Vec<Vec<Inline>>]);

fn table(parsed: &[Block]) -> TableParts<'_> {
    match &parsed[0] {
        Block::Table {
            align,
            header,
            rows,
        } => (align, header, rows),
        other => panic!("expected a table, got {other:?}"),
    }
}

/* ---- BlockParserSuite ---- */

#[test]
fn atx_headings_of_each_level() {
    assert_eq!(parse("# Hi"), vec![heading(1, vec![text("Hi")])]);
    assert_eq!(parse("### a b"), vec![heading(3, vec![text("a b")])]);
}

#[test]
fn a_closing_run_of_hashes_is_stripped() {
    assert_eq!(parse("# Hi #"), vec![heading(1, vec![text("Hi")])]);
}

#[test]
fn not_a_heading_no_space_or_seven_hashes() {
    assert_eq!(parse("#no"), vec![para("#no")]);
    assert_eq!(parse("####### x"), vec![para("####### x")]);
}

#[test]
fn thematic_breaks() {
    assert_eq!(parse("---"), vec![Block::ThematicBreak]);
    assert_eq!(parse("***"), vec![Block::ThematicBreak]);
    assert_eq!(parse("- - -"), vec![Block::ThematicBreak]);
}

#[test]
fn paragraph_joins_lines_with_a_soft_break() {
    assert_eq!(
        parse("a\nb"),
        vec![Block::Paragraph(vec![
            text("a"),
            Inline::SoftBreak,
            text("b")
        ])]
    );
}

#[test]
fn blank_line_separates_paragraphs() {
    assert_eq!(parse("a\n\nb"), vec![para("a"), para("b")]);
}

#[test]
fn setext_headings() {
    assert_eq!(parse("Title\n==="), vec![heading(1, vec![text("Title")])]);
    assert_eq!(parse("Title\n---"), vec![heading(2, vec![text("Title")])]);
}

#[test]
fn a_dashed_line_with_no_preceding_paragraph_is_a_thematic_break() {
    assert_eq!(parse("\n---"), vec![Block::ThematicBreak]);
}

#[test]
fn fenced_code_keeps_its_info_strings_first_word_as_the_language() {
    assert_eq!(
        parse("```scala\nval x = 1\n```"),
        vec![code(Some("scala"), "val x = 1", true)]
    );
    assert_eq!(
        parse("```rust title=a\nx\n```"),
        vec![Block::Code {
            info: Some("rust title=a".into()),
            lang: Some("rust".into()),
            code: "x".into(),
            fenced: true
        }]
    );
}

#[test]
fn tilde_fence() {
    assert_eq!(parse("~~~\nx\n~~~"), vec![code(None, "x", true)]);
}

#[test]
fn fenced_code_unterminated_at_eof_still_closes() {
    assert_eq!(parse("```\nx"), vec![code(None, "x", true)]);
}

#[test]
fn blank_lines_inside_a_fence_are_preserved() {
    assert_eq!(parse("```\na\n\nb\n```"), vec![code(None, "a\n\nb", true)]);
}

#[test]
fn indented_code_block() {
    assert_eq!(parse("    code"), vec![code(None, "code", false)]);
}

#[test]
fn blockquote_with_a_lazy_multi_line_paragraph() {
    assert_eq!(
        parse("> a\n> b"),
        vec![Block::Quote(vec![Block::Paragraph(vec![
            text("a"),
            Inline::SoftBreak,
            text("b")
        ])])]
    );
}

#[test]
fn nested_blockquote() {
    assert_eq!(
        parse("> > deep"),
        vec![Block::Quote(vec![Block::Quote(vec![para("deep")])])]
    );
}

#[test]
fn blockquote_containing_a_fenced_code_block() {
    assert_eq!(
        parse("> ```\n> x\n> ```"),
        vec![Block::Quote(vec![code(None, "x", true)])]
    );
}

#[test]
fn thematic_break_between_two_paragraphs() {
    assert_eq!(
        parse("a\n\n---\n\nb"),
        vec![para("a"), Block::ThematicBreak, para("b")]
    );
}

/* ---- ListSuite ---- */

fn item(blocks: Vec<Block>) -> ListItem {
    ListItem { blocks, task: None }
}

#[test]
fn bullet_list_tight() {
    assert_eq!(
        parse("- a\n- b"),
        vec![Block::List {
            ordered: false,
            start: 1,
            tight: true,
            items: vec![item(vec![para("a")]), item(vec![para("b")])]
        }]
    );
}

#[test]
fn bullet_markers_plus_and_star_each_form_their_own_list() {
    assert_eq!(parse("- a\n+ b").len(), 2);
    assert_eq!(parse("- a\n* b").len(), 2);
}

#[test]
fn ordered_list_keeps_its_start_ordinal() {
    assert_eq!(
        parse("3. a\n4. b"),
        vec![Block::List {
            ordered: true,
            start: 3,
            tight: true,
            items: vec![item(vec![para("a")]), item(vec![para("b")])]
        }]
    );
}

#[test]
fn ordered_with_paren_delimiter() {
    assert!(list(&parse("1) a")).0);
}

#[test]
fn nested_list_under_an_item() {
    let parsed = parse("- a\n  - b");
    let (_, _, _, items) = list(&parsed);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].blocks[0], para("a"));
    assert_eq!(
        items[0].blocks[1],
        Block::List {
            ordered: false,
            start: 1,
            tight: true,
            items: vec![item(vec![para("b")])]
        }
    );
}

#[test]
fn a_blank_line_between_items_makes_the_list_loose() {
    let parsed = parse("- a\n\n- b");
    let (_, _, tight, items) = list(&parsed);
    assert!(!tight);
    assert_eq!(items.len(), 2);
}

#[test]
fn a_blank_line_inside_an_item_makes_the_list_loose() {
    let parsed = parse("- a\n\n  b");
    let (_, _, tight, items) = list(&parsed);
    assert!(!tight);
    assert_eq!(items[0].blocks, vec![para("a"), para("b")]);
}

#[test]
fn task_list_items() {
    assert_eq!(
        list(&parse("- [ ] todo")).3[0],
        ListItem {
            blocks: vec![para("todo")],
            task: Some(false)
        }
    );
    assert_eq!(
        list(&parse("- [x] done")).3[0],
        ListItem {
            blocks: vec![para("done")],
            task: Some(true)
        }
    );
    assert_eq!(list(&parse("- [X] done")).3[0].task, Some(true));
}

#[test]
fn brackets_without_a_space_are_not_a_task() {
    assert_eq!(list(&parse("- [] x")).3[0].task, None);
}

#[test]
fn a_bullet_interrupts_a_paragraph_ordered_only_when_it_starts_at_1() {
    assert_eq!(parse("text\n- item").len(), 2);
    assert_eq!(parse("text\n1. item").len(), 2);
    // 不从 1 开始的有序标记只是段落文字。
    assert_eq!(
        parse("text\n2. item"),
        vec![Block::Paragraph(vec![
            text("text"),
            Inline::SoftBreak,
            text("2. item")
        ])]
    );
}

/* ---- TableSuite ---- */

fn cell(s: &str) -> Vec<Inline> {
    inline_parser::parse(s)
}

#[test]
fn a_minimal_two_column_table() {
    assert_eq!(
        parse("| a | b |\n|---|---|\n| 1 | 2 |"),
        vec![Block::Table {
            align: vec![Align::None, Align::None],
            header: vec![cell("a"), cell("b")],
            rows: vec![vec![cell("1"), cell("2")]],
        }]
    );
}

#[test]
fn column_alignment_from_the_delimiter_row() {
    let parsed = parse("| l | c | r |\n|:--|:-:|--:|\n| 1 | 2 | 3 |");
    assert_eq!(table(&parsed).0, [Align::Left, Align::Center, Align::Right]);
}

#[test]
fn a_table_without_outer_pipes() {
    let parsed = parse("a | b\n--- | ---\n1 | 2");
    let (_, header, rows) = table(&parsed);
    assert_eq!(header, [cell("a"), cell("b")]);
    assert_eq!(rows, [vec![cell("1"), cell("2")]]);
}

#[test]
fn ragged_rows_are_padded_and_over_long_rows_truncated() {
    let parsed = parse("| a | b |\n|---|---|\n| 1 |\n| 1 | 2 | 3 |");
    let rows = table(&parsed).2;
    assert_eq!(rows[0], vec![cell("1"), vec![]]);
    assert_eq!(rows[1], vec![cell("1"), cell("2")]);
}

#[test]
fn an_escaped_pipe_stays_inside_the_cell() {
    let parsed = parse("| a | b |\n|---|---|\n| x \\| y | z |");
    assert_eq!(table(&parsed).2[0][0], cell("x \\| y"));
}

#[test]
fn a_pipe_inside_a_code_span_does_not_split_the_cell() {
    let parsed = parse("| a | b |\n|---|---|\n| `x|y` | z |");
    assert_eq!(
        table(&parsed).2[0],
        vec![vec![Inline::Code("x|y".into())], cell("z")]
    );
}

#[test]
fn cells_carry_inline_formatting() {
    let parsed = parse("| a | b |\n|---|---|\n| **x** | _y_ |");
    assert_eq!(
        table(&parsed).2[0][0],
        vec![Inline::Strong(vec![text("x")])]
    );
}

#[test]
fn a_header_line_with_no_valid_delimiter_row_is_just_a_paragraph() {
    assert_eq!(
        parse("a | b\nnot a delimiter"),
        vec![Block::Paragraph(vec![
            text("a | b"),
            Inline::SoftBreak,
            text("not a delimiter")
        ])]
    );
}

/* ---- 移植时加的：Scala 字符串语义的边角 ---- */

#[test]
fn split_lines_normalises_crlf_and_lone_cr_and_drops_one_trailing_empty_line() {
    assert_eq!(split_lines(""), Vec::<&str>::new());
    assert_eq!(split_lines("\n"), vec![""]);
    assert_eq!(split_lines("a\r\nb\rc\n"), vec!["a", "b", "c"]);
    assert_eq!(split_lines("a\n\n"), vec!["a", ""]);
}

#[test]
fn unicode_decimal_digits_start_an_ordered_list_like_java_is_digit() {
    // Java 的 isDigit 认所有 Nd：全角与阿拉伯-印度数字也是序号，按位取值；上标 ² 不是。
    assert_eq!(list(&parse("１２. a")).1, 12);
    assert_eq!(list(&parse("٣) a")).1, 3);
    assert_eq!(parse("² a"), vec![para("² a")]);
    // 至多 9 位数字。
    assert_eq!(parse("1234567890. a"), vec![para("1234567890. a")]);
    assert_eq!(list(&parse("123456789. a")).1, 123_456_789);
}

#[test]
fn heading_content_is_trimmed_like_java_trim() {
    // Java 的 trim 去掉所有 ≤ U+0020 的字符，不去掉全角空格。
    assert_eq!(parse("# \u{1}x\u{1}"), vec![heading(1, vec![text("x")])]);
    assert_eq!(
        parse("# \u{3000}x"),
        vec![heading(1, vec![text("\u{3000}x")])]
    );
    assert_eq!(parse("# a ## ##"), vec![heading(1, vec![text("a ##")])]);
    assert_eq!(parse("# ##"), vec![heading(1, vec![text("##")])]);
}

#[test]
fn a_bullet_with_a_wide_character_keeps_its_content() {
    assert_eq!(list(&parse("- 😀 x")).3[0], item(vec![para("😀 x")]));
    assert_eq!(list(&parse("-\t中文")).3[0], item(vec![para("中文")]));
}
