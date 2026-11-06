//! 设置页的中性画法的用例（docs/specs/rust-cli/tui-kit.md §4.4 K5）：每个画法一个有代表性的输入。Puffin 的
//! settings/tests*.rs 经它的页与整屏测同样的规则；行的大小与 token 数的用例随画法从 Puffin 搬来。
use std::collections::BTreeMap;

use crate::app::layout::Layout;
use crate::render::line::StyledLine;
use crate::render::{Span, Style};
use crate::theme::rgb::Rgb;
use crate::theme::theme::Theme;

use super::hints::{self, Asking, Chosen, Enter, Keys, Space};
use super::rules::{self, Behavior, Rule, Source};
use super::view::{self, Mode, Page, PageCtx, Search};
use super::{Action, Applies, Ink, Item, Opt, Part, Row, Text, mcp_status_parts, size};
use super::{McpStatus, tokens_short, wrap};

fn rich() -> Theme {
    Theme::from(Rgb::new(214, 216, 220), Rgb::new(21, 22, 26))
}

fn plain(lines: &[StyledLine]) -> Vec<String> {
    lines
        .iter()
        .map(|l| l.plain().trim_end().to_owned())
        .collect()
}

fn ctx<'a>(theme: &'a Theme, mode: Mode<'a>, age: &'a dyn Fn(usize) -> i64) -> PageCtx<'a> {
    PageCtx {
        theme,
        editor: "",
        mode,
        sign_in: None,
        test: None,
        age,
    }
}

fn settled(_: usize) -> i64 {
    i64::MAX / 2
}

#[test]
fn sizes_and_token_counts_as_the_rows_say_them() {
    assert_eq!(size(640), "640 B");
    assert_eq!(size(2150), "2.1 KB");
    assert_eq!(tokens_short(131_072), "131k");
    assert_eq!(tokens_short(1_000_000), "1M");
}

#[test]
fn a_word_longer_than_the_line_keeps_its_end() {
    assert_eq!(wrap("one two three", 7), ["one two", "three"]);
    assert_eq!(wrap("/a/b/very-long-name", 8), ["…ng-name"]);
}

#[test]
fn every_row_kind_lines_up_and_the_chosen_row_explains_itself() {
    let t = rich();
    let opts = [Opt::new("dark", "Dark"), Opt::new("light", "Light")];
    let status = [Part::new("connected", Ink::Ok)];
    let rows = [
        Row::Heading("Look"),
        Row::Switch {
            label: "Auto-compact",
            on: true,
            desc: "Compacts when the context is full.",
            applies: Applies::Now,
        },
        Row::Choice {
            label: "Mode",
            options: &opts,
            current: "dark",
            default: "dark",
            desc: "",
            applies: Applies::Look,
        },
        Row::Text(Text {
            label: "Key",
            value: "sk-12345678",
            desc: "",
            applies: Applies::Next,
            unit: "",
            empty: "not set",
            secret: true,
            note: "",
        }),
        Row::Item(Item {
            name: "github",
            toggle: Some(true),
            scope: "user",
            status: &status,
            desc: "",
            opens: true,
            detail: &[],
        }),
        Row::Action(Action {
            label: "Add a rule",
            desc: "",
            plus: true,
            danger: false,
            aside: "",
            off: false,
            signs_in: false,
        }),
    ];
    let age = settled;
    let c = ctx(&t, Mode::Browse, &age);
    let (lines, at, end) = view::page_lines(&rows, Some(0), 60, &c);
    assert_eq!(
        plain(&lines),
        [
            "Look",
            "Auto-compact   on",
            "  Compacts when the context is full. Applies at once.",
            "Mode           Dark  default",
            "Key            ••••••••5678",
            "■ github   user   connected",
            "+ Add a rule",
        ]
    );
    assert_eq!((at, end), (Some(1), Some(2)));
    // 选中的项行尾有 ›；方块是 brand。
    let (lines, ..) = view::page_lines(&rows, Some(3), 60, &c);
    assert_eq!(
        lines[4].plain().trim_end(),
        "■ github   user   connected  ›"
    );
    assert_eq!(lines[4].spans[0].style, Style::fg(t.brand));
}

#[test]
fn a_choice_opens_its_options_and_a_remove_asks_under_the_row() {
    let t = rich();
    let opts = [
        Opt::new("a", "Ask").desc("asks first"),
        Opt::new("b", "Never"),
    ];
    let rows = [
        Row::Choice {
            label: "Kind",
            options: &opts,
            current: "a",
            default: "",
            desc: "",
            applies: Applies::Form,
        },
        Row::Action(Action {
            label: "Remove this rule",
            desc: "",
            plus: false,
            danger: true,
            aside: "",
            off: false,
            signs_in: false,
        }),
    ];
    let age = settled;
    let c = ctx(&t, Mode::Choosing { row: 0, at: 1 }, &age);
    let (lines, ..) = view::page_lines(&rows, Some(0), 60, &c);
    assert_eq!(
        plain(&lines[..3]),
        ["Kind   Ask", "  Ask     asks first  current", "  Never"]
    );
    let c = ctx(
        &t,
        Mode::Confirming {
            row: 1,
            at: 1,
            label: "Remove",
            note: "Your mode decides again.",
        },
        &age,
    );
    let (lines, ..) = view::page_lines(&rows, Some(1), 60, &c);
    assert_eq!(
        plain(&lines[1..]),
        [
            "Remove this rule",
            "  Your mode decides again.",
            "  Remove",
            "  Keep it"
        ]
    );
    assert_eq!(lines[1].spans[0].style, Style::fg(t.err));
}

#[test]
fn the_body_has_the_head_the_directory_from_col_3_and_the_page_from_col_16() {
    let t = rich();
    let rows = [Row::Switch {
        label: "Auto-compact",
        on: false,
        desc: "",
        applies: Applies::Now,
    }];
    let crumbs = vec!["Machines".to_owned(), "gpu-1".to_owned()];
    let age = settled;
    let c = ctx(&t, Mode::Browse, &age);
    let page = Page {
        section: 0,
        crumbs: &crumbs,
        desc: "How the agent works.",
        rows: &rows,
    };
    let out = plain(&view::body(&page, None, None, 100, 20, &c, true));
    assert_eq!(out[1], "   Settings  ›  General  ›  Machines  ›  gpu-1");
    assert_eq!(out[3], "   General      How the agent works.");
    assert_eq!(out[5], "   Models       Auto-compact   off");
    assert_eq!(out[8], "", "a blank row between the two groups");
    assert_eq!(out[9], "   Subagents");
    assert_eq!(out.len(), 20);
    // 窄于 76 列：没有目录，页从第 3 列起。
    let narrow = plain(&view::body(&page, None, None, 60, 8, &c, false));
    assert_eq!(narrow[3], "   How the agent works.");
    // 搜索：头是打的词与命中数，有命中的段带数。
    let hits = BTreeMap::from([("models", 2usize)]);
    let search = Search {
        word: " gpu ",
        hits: &hits,
    };
    let found = plain(&view::body(&page, None, Some(search), 100, 8, &c, false));
    assert_eq!(found[1], "   Settings  ›  “gpu”  2 matches");
    assert_eq!(found[5], "   Models    2  Auto-compact   off");
}

#[test]
fn a_long_page_says_how_many_lines_are_above_and_below() {
    let t = rich();
    let lines: Vec<StyledLine> = (0..20)
        .map(|i| StyledLine::new(vec![Span::plain(format!("row {i}"))]))
        .collect();
    let shown = plain(&view::windowed(&t, lines, 6, 10, 10));
    assert_eq!(shown.first().map(String::as_str), Some("↑ 8 more"));
    assert_eq!(shown.last().map(String::as_str), Some("↓ 7 more"));
    assert_eq!(shown.len(), 6);
}

#[test]
fn the_keys_follow_what_the_page_is_doing() {
    let keys = |m, c| {
        hints::keys(m, c, false, "vim", false)
            .into_iter()
            .map(|(k, v)| format!("{k} {v}"))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        keys(Keys::Page { stacked: false }, Some(Chosen::Editor)),
        ["↑↓ choose", "← sections", "⏎ edit in vim", "esc close"]
    );
    assert_eq!(
        keys(
            Keys::Page { stacked: true },
            Some(Chosen::Item {
                space: Space::Word("connect"),
                remove: true,
                open: Some(""),
            })
        ),
        [
            "↑↓ choose",
            "← sections",
            "space connect",
            "⌫ remove",
            "⏎ open",
            "esc back"
        ]
    );
    assert_eq!(
        keys(
            Keys::Page { stacked: false },
            Some(Chosen::Action(Enter::Off))
        ),
        ["↑↓ choose", "← sections", "esc close"]
    );
    assert_eq!(keys(Keys::Editing { stuck: true }, None), ["esc cancel"]);
    assert_eq!(
        keys(Keys::Directory, None),
        ["↑↓ section", "→ open", "esc close"]
    );
}

#[test]
fn the_said_line_is_the_ask_a_failure_or_when_a_change_applies() {
    let t = rich();
    let text = |els: Vec<crate::app::element::Element>| {
        els.iter()
            .flat_map(|e| plain(&Layout::lay(e, 80)))
            .collect::<Vec<_>>()
    };
    let asking = hints::asking_line(Asking::Permission);
    assert_eq!(
        text(hints::said(&t, None, Some(asking))),
        [
            "",
            " ● The agent is asking for permission. esc goes back to answer it."
        ]
    );
    assert_eq!(
        text(hints::said(&t, Some(Err("Could not save: no")), None)),
        ["", " ✗ Could not save: no"]
    );
    assert_eq!(
        text(hints::said(&t, Some(Ok(Applies::Next.saved())), None)),
        [
            "",
            "   Saved. New sessions use it; this one keeps the old setting."
        ]
    );
    let grown: String = hints::editing_row(&t, "API key", true, "")
        .iter()
        .map(|s| s.text.as_ref())
        .collect();
    assert_eq!(grown, "API key  hidden as you type");
    assert_eq!(
        hints::value_placeholder(true, true, ""),
        "Type or paste the key"
    );
}

#[test]
fn rules_are_said_as_their_phrase_kind_and_meaning() {
    let bash = Rule {
        tool: "Bash",
        content: Some("cargo test:*"),
        text: "Bash(cargo test:*)",
    };
    assert_eq!(rules::phrase(bash), "cargo test …");
    assert_eq!(rules::kind_word("Bash"), "command");
    assert_eq!(rules::source_word(Source::LocalSettings), "this project");
    assert_eq!(
        rules::sentence(Behavior::Allow, bash),
        "Allows any command that starts with cargo test, without asking."
    );
    assert_eq!(
        rules::page_desc(Behavior::Deny, bash),
        "Stops the agent from running any command that starts with cargo test; this wins over the other rules."
    );
    assert_eq!(
        rules::removed_said(Behavior::Allow, bash),
        "Removed. The agent asks before cargo test again, from its next tool call."
    );
    let mcp = Rule {
        tool: "mcp__github__get_issue",
        content: None,
        text: "mcp__github__get_issue",
    };
    assert_eq!(rules::phrase(mcp), "github · get_issue");
    assert_eq!(
        rules::means(mcp)
            .iter()
            .map(|p| p.text.as_str())
            .collect::<String>(),
        "the tool get_issue of github"
    );
    assert_eq!(
        mcp_status_parts(McpStatus::Connected { tools: 1 })[1].text,
        " · 1 tool"
    );
}
