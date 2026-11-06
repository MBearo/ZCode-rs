//! 流式回答：`Written` 标记、笔尖、隐藏的词尾、开着的代码块、到达中的表格与落定。
//! 移植自 packages/tui/tui/test/src/zcode/tui/markdown/MarkdownRenderSuite.scala（feat/tui a2bf878b34）。
use crate::markdown::render::{HIDDEN, SETTLED, Tail, answer, render, rides_tail, waits};
use crate::render::{Color, Span, Style, StyledLine, attr};
use crate::theme::ease;
use crate::theme::theme::Theme;

use super::{doc, laid, lines, lines_at, rich_theme, v};

/// 流式回答的尾巴，笔尖 `█` 用 brand（§4.2「笔」）。
fn nib() -> Span {
    Span::new("█", Style::fg(rich_theme().brand))
}

fn streaming() -> Tail {
    Tail::new(Some(nib()))
}

fn written(sp: &Span) -> bool {
    sp.style.has_attr(attr::WRITTEN)
}

fn plain(ls: &[StyledLine]) -> Vec<String> {
    ls.iter().map(StyledLine::plain).collect()
}

#[test]
fn a_streaming_answers_letters_are_in_their_own_ink_marked_as_written_and_the_nib_ends_it() {
    let rich = rich_theme();
    let ls = laid(&answer(&doc("hello `x`"), Some(&streaming()), &rich), 60);
    let last = ls.last().unwrap();
    assert_eq!(last.plain(), "   hello `x`█");
    assert_eq!(last.spans.last().unwrap(), &nib());
    let hello = last
        .spans
        .iter()
        .find(|sp| sp.text.contains("hello"))
        .unwrap();
    assert!(written(hello), "{hello:?}");
    assert_eq!(
        hello.style.fg_color(),
        Color::Default,
        "no tint: their settling is the caller's"
    );
    let x = last.spans.iter().find(|sp| sp.text == "x").unwrap();
    assert_eq!(x.style.fg_color(), rich.code_ink.fg_color());
    assert!(
        !last
            .spans
            .iter()
            .filter(|sp| sp.text.starts_with("   "))
            .any(written),
        "the indent is not the agent's writing"
    );
}

#[test]
fn a_committed_answer_has_no_nib_and_nothing_marked_as_written() {
    let ls = laid(&answer(&doc("hello"), None, &rich_theme()), 60);
    assert!(!ls.iter().any(|l| l.plain().contains('█')));
    assert!(!ls.iter().any(|l| l.spans.iter().any(written)));
    assert_eq!(
        lines(&answer(
            &doc("hello"),
            Some(&Tail::default()),
            &Theme::PLAIN
        )),
        v(&["   hello"]),
        "no nib given, none drawn"
    );
}

#[test]
fn the_nib_rides_trailing_prose_code_a_table_arriving_or_a_list_ending_in_one() {
    assert!(rides_tail(&doc("")));
    assert!(rides_tail(&doc("one\n\ntwo")));
    assert!(rides_tail(&doc("# Title")));
    assert!(rides_tail(&doc("- item")));
    assert!(rides_tail(&doc("text\n\n```\ncode")));
    assert!(rides_tail(&doc("- item\n\n  ```\n  code")));
    assert!(rides_tail(&doc("| a |\n|---|\n| 1 |")));
    assert!(!rides_tail(&doc("> quoted")));
}

#[test]
fn only_the_trailing_block_carries_the_nib_while_streaming_every_blocks_letters_are_written() {
    let ls = laid(
        &answer(
            &doc("first para\n\nsecond para"),
            Some(&streaming()),
            &rich_theme(),
        ),
        60,
    );
    assert!(!ls[0].plain().contains('█'), "{}", ls[0].plain());
    assert!(ls.last().unwrap().plain().contains('█'));
    assert!(
        ls[0].spans.iter().any(written),
        "the newest letters are counted across block boundaries"
    );
}

#[test]
fn the_last_item_of_a_list_carries_the_nib() {
    assert_eq!(
        lines(&answer(
            &doc("- one\n- two"),
            Some(&streaming()),
            &Theme::PLAIN
        )),
        v(&["   • one", "   • two█"])
    );
}

#[test]
fn the_hidden_rest_of_a_word_holds_its_cells() {
    // 宽 17："aaaa bbbb cc" 放得下，但整词 "cccccc" 放不下，所以 cc 从第二行开始。
    let src = format!("aaaa bbbb cc{HIDDEN}cccc");
    let ls = lines_at(&answer(&doc(&src), Some(&streaming()), &Theme::PLAIN), 17);
    let trimmed: Vec<String> = ls
        .iter()
        .map(|l| l.replace('\u{a0}', " ").trim_end().to_owned())
        .collect();
    assert_eq!(trimmed, v(&["   aaaa bbbb", "   cc█"]));
    assert_eq!(
        ls.last().unwrap().chars().count(),
        3 + 6,
        "the nib and three blanks stand for the four hidden letters"
    );
    // 没有笔尖时，四个隐藏的字母全是空白，也不算 agent 写的字。
    let bare = laid(
        &answer(
            &doc(&format!("ab{HIDDEN}cd")),
            Some(&Tail::default()),
            &rich_theme(),
        ),
        60,
    );
    assert_eq!(plain(&bare), v(&["   ab\u{a0}\u{a0}"]));
    assert!(
        !bare[0]
            .spans
            .iter()
            .filter(|sp| sp.text.contains('\u{a0}'))
            .any(written)
    );
}

#[test]
fn a_hidden_word_tail_inside_emphasis_keeps_the_emphasis() {
    let src = format!("**bo{HIDDEN}ld** x");
    let ls = laid(&answer(&doc(&src), Some(&streaming()), &rich_theme()), 60);
    assert!(
        ls[0]
            .spans
            .iter()
            .any(|sp| sp.text.contains('b') && sp.style.has_attr(attr::BOLD)),
        "{:?}",
        ls[0].spans
    );
    let p = ls[0].plain();
    assert!(!p.contains('*') && !p.contains("ld"), "{p}");
}

#[test]
fn an_open_code_block_shows_its_code_up_to_the_playhead_the_nib_in_the_code() {
    let src = format!("```\nval x = 1{HIDDEN}23");
    let ls = lines(&answer(&doc(&src), Some(&streaming()), &Theme::PLAIN));
    assert!(
        ls.iter().any(|l| l.contains("val x = 1█")),
        "{}",
        ls.join("|")
    );
    assert!(
        !ls.iter().any(|l| l.contains("123")) && !ls.iter().any(|l| l.contains(HIDDEN)),
        "{}",
        ls.join("|")
    );
}

#[test]
fn rich_a_code_block_being_written_is_on_the_panel_with_no_bottom_edge_until_the_answer_ends() {
    let rich = rich_theme();
    let panel = rich.panel.unwrap();
    let src = "```\nval x = 1\nval y = 2";
    let open = laid(&answer(&doc(src), Some(&streaming()), &rich), 30);
    let heads: Vec<String> = open
        .iter()
        .map(|l| l.plain().chars().take(5).collect())
        .collect();
    assert_eq!(heads, v(&["   ▄▄", "    v", "    v"]));
    // 正在写的那一行不再铺亮带：字下面什么都不画（§4.2「笔」）。
    assert!(
        open[1..]
            .iter()
            .all(|l| l.spans[1..].iter().all(|sp| sp.style.bg_color() == panel)),
        "{open:?}"
    );
    assert!(
        open.last().unwrap().spans.iter().any(|sp| sp.text == "█"),
        "the nib is in the code"
    );
    let ended = laid(
        &answer(&doc(src), Some(&Tail::default().with_ended(true)), &rich),
        30,
    );
    let last = ended.last().unwrap().plain();
    assert!(last.trim().starts_with('▀'), "{last}");
}

#[test]
fn a_table_still_arriving_at_the_end_is_not_drawn_the_nib_waits_once_complete_it_is() {
    let table = "| a | b |\n|---|---|\n| 1 | 2 |";
    let src = format!("intro\n\n{table}");
    let open = lines(&answer(&doc(&src), Some(&streaming()), &Theme::PLAIN));
    assert_eq!(open, v(&["   intro", "", "   █"]));
    let ended = lines(&answer(
        &doc(&src),
        Some(&Tail::default().with_ended(true)),
        &Theme::PLAIN,
    ));
    assert!(ended.iter().any(|l| l.contains('1')), "{}", ended.join("|"));
    let closed_src = format!("intro\n\n{table}\n\nafter");
    let closed = laid(
        &answer(&doc(&closed_src), Some(&streaming()), &rich_theme()),
        60,
    );
    assert!(
        closed.iter().any(|l| l.plain().contains('1'))
            && closed.last().unwrap().plain().contains("after"),
        "{:?}",
        plain(&closed)
    );
    assert!(
        !closed
            .iter()
            .filter(|l| l.plain().contains('1'))
            .any(|l| l.spans.iter().any(written)),
        "a table lands whole, not letter by letter"
    );
}

#[test]
fn rich_a_table_laid_whole_appears_whole_and_settles_once_every_cell_together() {
    let rich = rich_theme();
    let src = "intro\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nafter";
    let at = |age: i64| {
        let t = Tail::default().with_landed(move |i| if i == 1 { age } else { SETTLED });
        laid(&answer(&doc(src), Some(&t), &rich), 60)
    };
    let whole = laid(&render(&doc(src), &rich), 60);
    // 每一帧都是整张表格，只有墨色在变（§4.2「笔」的「表格」）。
    assert_eq!(plain(&at(0)), plain(&whole));
    let ink = |ls: &[StyledLine], ch: &str| {
        ls.iter()
            .flat_map(|l| l.spans.iter())
            .find(|sp| sp.text.contains(ch))
            .unwrap()
            .style
            .fg_color()
    };
    assert_ne!(ink(&at(0), "1"), ink(&whole, "1"), "it starts lighter");
    assert_eq!(
        ink(&at(0), "a") == ink(&whole, "a"),
        ink(&at(0), "1") == ink(&whole, "1"),
        "the header and the cells settle together"
    );
    assert_eq!(
        ink(&at(ease::TICKS), "1"),
        ink(&whole, "1"),
        "settled after 4 ticks"
    );
    assert_eq!(ink(&at(0), "after"), ink(&whole, "after"), "only the table");
}

#[test]
fn a_paragraph_that_may_be_a_tables_header_is_not_drawn_while_it_arrives() {
    // 以前表头先按原样打出 `| a | b |`，分隔行一到又整行消失（§4.2「流式节奏」的「表格」）。
    for src in [
        "intro\n\n| a | b",
        "intro\n\n| a | b |\n|--",
        "- intro\n\n  | a | b",
        "> | a |",
    ] {
        let open = lines(&answer(&doc(src), Some(&streaming()), &Theme::PLAIN));
        assert!(
            !open.iter().any(|l| l.contains("a |")) && !open.iter().any(|l| l.contains("| a")),
            "{src}: {}",
            open.join("|")
        );
        assert!(
            open.last().unwrap().trim().ends_with('█'),
            "{src}: {}",
            open.join("|")
        );
    }
    assert!(waits(&doc("intro\n\n| a | b")));
    assert!(!waits(&doc("intro\n\na | b")));
    let ended = lines(&answer(
        &doc("intro\n\n| not a table"),
        Some(&Tail::default().with_ended(true)),
        &Theme::PLAIN,
    ));
    assert!(
        ended.iter().any(|l| l.contains("| not a table")),
        "{}",
        ended.join("|")
    );
}
