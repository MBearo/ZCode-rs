//! transcript 内容的画法（docs/specs/rust-cli/tui-kit.md §4.4）：每个从 Puffin 搬来的画法一个有代表性的中性输入，铺出来看
//! 行。Puffin 的 tool_view_tests、failures_tests、notices_tests、shell_view_tests、view_tests_* 经它的条目映射与整屏测同样
//! 的规则与颜色；子代理的列与失败块的 `at` 时刻的纯用例随画法从 Puffin 搬来。
use serde_json::{Value, json};

use crate::app::element::Element;
use crate::app::layout::Layout;
use crate::app::widgets::entries::{self, Compaction};
use crate::app::widgets::failure_block::{self, Failure, Names};
use crate::app::widgets::gather::{self, Member, Thought};
use crate::app::widgets::notices::{self, Notice, Work};
use crate::app::widgets::roster;
use crate::app::widgets::shell_block::{self, ShellRun, ShellStatus};
use crate::app::widgets::subagent_cols::{self, Cells};
use crate::app::widgets::todo_step::{self, Status as Step};
use crate::app::widgets::tool_row::{self, ToolClock, ToolCtx};
use crate::app::widgets::tools::{
    Call, Changes, Facts, NumberedLine, Sign, Status, Where, no_todos,
};
use crate::app::widgets::user_slab::{self, Piece};
use crate::app::widgets::welcome;
use crate::markdown::render::SETTLED;
use crate::render::line::StyledLine;
use crate::render::width::string_width;
use crate::render::{Span, Style};
use crate::theme::rgb::Rgb;
use crate::theme::theme::Theme;

fn rich() -> Theme {
    Theme::from(Rgb::new(214, 216, 220), Rgb::new(21, 22, 26))
}

fn here() -> Where {
    Where {
        cwd: "/Users/dev/project".to_owned(),
        home: Some("/Users/dev".to_owned()),
    }
}

fn laid(el: &Element, w: usize) -> Vec<StyledLine> {
    Layout::lay(el, w)
}

fn lines(el: &Element, w: usize) -> Vec<String> {
    laid(el, w)
        .iter()
        .map(|l| l.plain().trim_end().to_owned())
        .collect()
}

fn colour(line: &StyledLine, text: &str) -> Option<Style> {
    line.spans
        .iter()
        .find(|sp| sp.text.contains(text))
        .map(|sp| sp.style)
}

fn text(spans: &[Span]) -> String {
    spans.iter().map(|sp| sp.text.as_ref()).collect()
}

fn done<'a>(id: &'a str, tool: &'a str, input: &'a Value) -> Call<'a> {
    Call {
        id,
        name: tool,
        summary: "",
        status: Status::Done,
        response: "r1",
        ended: true,
        facts: Some(Facts::named(tool, input)),
    }
}

/// 画调用要的：rich 主题、落定的 tick、没有远端与子代理的数字。
fn with_ctx<R>(details: bool, f: impl FnOnce(&ToolCtx<'_>) -> R) -> R {
    let w = here();
    let cells = |_: &Call<'_>, _: Option<i64>| Cells::default();
    let ctx = ToolCtx {
        theme: rich(),
        where_: &w,
        tasks: &|_| None,
        now: 1_000,
        now_ms: 0.0,
        details,
        todos: &no_todos,
        steps: &|_| None,
        cells: &cells,
        machine: &|_| None,
        pointed: None,
    };
    f(&ctx)
}

#[test]
fn a_finished_edit_is_its_row_and_its_numbered_diff_on_tinted_grounds() {
    let input = json!({"file_path": "/Users/dev/project/host/src/asks.ts"});
    let diff = [
        NumberedLine {
            sign: Sign::Remove,
            text: "old".to_owned(),
            line: 40,
        },
        NumberedLine {
            sign: Sign::Add,
            text: "new".to_owned(),
            line: 40,
        },
    ];
    let mut c = done("t1", "Edit", &input);
    if let Some(f) = c.facts.as_mut() {
        f.diff = Some(Changes {
            added: 1,
            removed: 1,
            lines: &diff,
        });
    }
    let el = with_ctx(false, |ctx| {
        tool_row::call(ctx, &c, &ToolClock::default(), None)
    });
    assert_eq!(
        lines(&el, 80),
        [
            " ✓ Edit  host/src/asks.ts  +1 −1",
            "   │   40 − old",
            "   │   40 + new"
        ]
    );
    let t = rich();
    let rows = laid(&el, 80);
    assert_eq!(colour(&rows[0], "✓").map(Style::fg_color), Some(t.ok));
    assert_eq!(colour(&rows[2], "new").map(Style::bg_color), t.add_bg);
}

#[test]
fn reads_in_a_closed_run_are_one_explored_row_with_their_counts() {
    let a = json!({"file_path": "/Users/dev/project/a.ts"});
    let b = json!({"file_path": "/Users/dev/project/b.ts"});
    let calls = [done("t1", "Read", &a), done("t2", "Read", &b)];
    let members: Vec<Member<'_>> = calls.iter().copied().map(Member::Call).collect();
    assert!(gather::shows(&members));
    let el = with_ctx(false, |ctx| {
        gather::line(
            ctx,
            &members,
            &|_| ToolClock::default(),
            Some(0),
            true,
            None,
            &|_| None,
        )
    });
    assert_eq!(lines(&el, 80), [" ✓ Explored 2 files"]);
    // 只有思考的一段也显示；只有不画的调用（ToolSearch）不显示。
    let none = json!({});
    let thought = [Member::Thought(Thought {
        id: "h1",
        text: "Looking",
        ended: true,
        duration_ms: Some(2_000),
    })];
    assert!(gather::shows(&thought));
    assert!(!gather::shows(&[Member::Call(done(
        "t3",
        "ToolSearch",
        &none
    ))]));
}

#[test]
fn a_thought_s_ticker_shows_the_page_of_its_paragraph_being_written() {
    assert_eq!(
        gather::pages("one two three four", 9),
        vec![(0, 7), (8, 13), (14, 18)]
    );
    assert_eq!(gather::ticker("one two three four", 18, 9), "four");
}

#[test]
fn a_batch_with_a_running_call_is_a_roster_one_row_per_call() {
    let a = json!({"file_path": "/Users/dev/project/a.ts"});
    let cmd = json!({"command": "make"});
    let mut running = done("t2", "Bash", &cmd);
    running.ended = false;
    running.status = Status::Running;
    let calls = [done("t1", "Read", &a), running];
    let batch = roster::of(&calls);
    assert_eq!(batch.len(), 2);
    assert!(roster::running(&batch[1]));
    assert!(
        roster::of(&calls[..1]).is_empty(),
        "one call is not a roster"
    );
    let rows = with_ctx(false, |ctx| {
        roster::rows(ctx, &batch, &|_| ToolClock::default(), None)
    });
    let shown: Vec<String> = rows.iter().flat_map(|r| lines(r, 80)).collect();
    assert!(
        shown
            .iter()
            .any(|l| l.contains("Read") && l.contains("a.ts")),
        "{shown:?}"
    );
    assert!(shown.iter().any(|l| l.contains("make")), "{shown:?}");
}

#[test]
fn a_refused_sign_in_says_who_refused_the_agent_s_words_and_the_next_step() {
    let it = Failure {
        code: "provider_not_configured",
        message: "Provider authentication failed.",
        reason: "auth_failed",
        http_status: 401,
        provider_id: "plan",
        model_id: "GLM-5.3",
        error_phase: "stream",
        request_id: "e28cc438",
        resend: true,
        at: 5_000,
        ..Failure::default()
    };
    let n = Names {
        provider: "BigModel".to_owned(),
        host: "open.bigmodel.cn".to_owned(),
        signs_in: true,
        model: "GLM-5.3".to_owned(),
    };
    assert!(!failure_block::retries(&it));
    assert_eq!(
        failure_block::sentence(&it, &n),
        "BigModel did not accept your sign-in."
    );
    let el = failure_block::block(&rich(), &it, &n, false, false, SETTLED, None, 0);
    let shown = lines(&el, 80);
    assert_eq!(shown[0], " ✗ BigModel did not accept your sign-in.");
    assert_eq!(shown[1], "   Provider authentication failed.  ·  HTTP 401");
}

#[test]
fn the_at_row_is_local_time() {
    assert_eq!(failure_block::clock_of(5000.0, 0), "00:00:05");
    assert_eq!(failure_block::clock_of(5000.0, 8 * 60), "08:00:05");
    assert_eq!(failure_block::clock_of(5000.0, -60), "23:00:05");
}

#[test]
fn a_background_command_that_finished_is_one_quiet_row() {
    let n = Notice::Finished {
        work: Work::Command,
        title: "Run the test suite",
    };
    let el = notices::entry(&rich(), &n, 1.0);
    assert_eq!(lines(&el, 80), [" • Command  Run the test suite  finished"]);
    assert_eq!(lines(&el, 24), [" • Command  finished"]);
}

#[test]
fn a_shell_command_that_failed_says_its_exit_and_time_and_counts_the_earlier_lines() {
    let out: Vec<String> = (1..=12).map(|k| format!("line {k}")).collect();
    let r = ShellRun {
        command: "make",
        status: ShellStatus::Done,
        ended: true,
        exit_code: 1,
        lines: &out,
        dropped: 0,
        started_at: 1_000,
        ended_at: 4_200,
        error: "",
    };
    assert!(r.longer());
    assert_eq!(r.total(), 12);
    let el = shell_block::block(&rich(), &r, &ToolClock::default(), 1_000, 0.0, false);
    let shown = lines(&el, 40);
    // rich 主题有板色：命令行上下是板的 `▄` 与 `▀` 边。
    assert_eq!(shown.len(), 3 + 1 + shell_block::TAIL, "{shown:?}");
    assert!(shown[0].chars().all(|c| c == '▄'), "{shown:?}");
    assert_eq!(shown[1], " ! make                   exit 1 · 3.2s");
    assert_eq!(string_width(&shown[1]), 39, "the outcome ends one cell in");
    assert_eq!(shown[3], "   │ … 2 earlier lines");
    assert_eq!(shown[4], "   │ line 3");
    assert_eq!(shown.last().map(String::as_str), Some("   │ line 12"));
    assert_eq!(lines(&shell_block::held(&rich(), "ls"), 40)[1], " ! ls");
}

#[test]
fn a_compaction_reads_like_a_call_and_a_stop_is_one_quiet_line() {
    let t = rich();
    assert_eq!(
        lines(
            &entries::compact_row(&t, Compaction::Success, Some(52_300), Some(7_900)),
            80
        ),
        [" ✓ Compact  52.3k → 7.9k tokens"]
    );
    assert_eq!(
        lines(
            &entries::compact_row(&t, Compaction::Running, None, None),
            80
        ),
        [" ○ Compact  summarising the conversation"]
    );
    assert_eq!(
        lines(&entries::stopped_line(&t, false), 80),
        ["   Stopped."]
    );
    assert_eq!(
        lines(&entries::stopped_line(&t, true), 80),
        ["   Stopped. The goal is paused; /goal resume picks it up."]
    );
    assert_eq!(
        lines(&entries::quiet_line(&t, "Queued."), 80),
        ["   Queued."]
    );
}

#[test]
fn an_ended_thought_drawn_alone_is_its_title_and_its_words_on_the_rail() {
    let el = entries::thought(
        &rich(),
        "Check the spec.",
        true,
        Some(6_000),
        None,
        None,
        0,
        false,
    );
    assert_eq!(lines(&el, 80), ["   Thought · 6s", "   │ Check the spec."]);
}

#[test]
fn a_tall_turn_shows_its_first_six_rows_and_its_last_three() {
    let t = rich();
    let body: String = (1..=20)
        .map(|k| format!("row {k}"))
        .collect::<Vec<_>>()
        .join("\n");
    let words = user_slab::words(&t, "", &[Piece::Text(body)]);
    let el = user_slab::turn(&t, Some(words.clone()), Vec::new(), false, Vec::new());
    let shown = lines(&el, 80);
    let rows: Vec<&String> = shown.iter().filter(|l| l.contains("row ")).collect();
    assert_eq!(rows.len(), user_slab::SLAB_HEAD + user_slab::SLAB_TAIL);
    assert!(rows[0].ends_with("› row 1"), "{shown:?}");
    assert!(rows[5].ends_with("row 6"), "{shown:?}");
    assert!(rows[6].ends_with("row 18"), "{shown:?}");
    let open = user_slab::turn(&t, Some(words), Vec::new(), true, Vec::new());
    assert_eq!(
        lines(&open, 80)
            .iter()
            .filter(|l| l.contains("row "))
            .count(),
        20
    );
}

#[test]
fn a_command_name_is_brand_and_a_mention_takes_its_kind_s_ink() {
    let t = rich();
    let words = user_slab::words(
        &t,
        "/review",
        &[
            Piece::Text(" look at ".to_owned()),
            Piece::Mention("@a.ts".to_owned(), "file"),
        ],
    );
    let el = user_slab::turn(&t, Some(words), Vec::new(), false, Vec::new());
    let row = laid(&el, 80)
        .into_iter()
        .find(|l| l.plain().contains("/review"))
        .expect("有文字行");
    assert_eq!(colour(&row, "/review").map(Style::fg_color), Some(t.brand));
    assert_eq!(colour(&row, "@a.ts").map(Style::fg_color), Some(t.file));
    assert_eq!(user_slab::mention_ink(&t, "skill"), t.skill);
}

#[test]
fn the_welcome_block_names_the_version_and_the_folder() {
    let t = rich();
    let el = welcome::block(&t, "0.0.0", "~/project", &[], &[], Some("0.17.0"));
    let shown = lines(&el, 100);
    assert_eq!(shown[0].trim(), "");
    assert_eq!(
        shown[1].trim_end(),
        "  ZCode  0.0.0   0.17.0 is out · zcode update"
    );
    assert_eq!(shown[2].trim_end(), "  ~/project");
    assert_eq!(shown.len(), 4, "{shown:?}");
    let done = welcome::done_row(&t, "Updated from 0.16.0.".to_owned());
    assert_eq!(text(&done), "✓ Updated from 0.16.0.");
    let said = lines(&welcome::block(&t, "0.0.0", "~/p", &done, &[], None), 100);
    assert_eq!(said[4].trim_end(), "  ✓ Updated from 0.16.0.");
    assert_eq!(said.len(), 6, "{said:?}");
    let resumed = welcome::resumed_said(
        &t,
        "0.0.0",
        "~/p",
        "Fix the build",
        "today 09:12",
        None,
        welcome::is_out,
    );
    let shown: Vec<String> = lines(&resumed, 80)
        .iter()
        .map(|l| l.trim_end().to_owned())
        .collect();
    assert_eq!(
        shown,
        [
            "",
            "  ZCode  0.0.0",
            "  ~/p",
            "  Fix the build  started today 09:12",
            ""
        ]
    );
}

#[test]
fn the_columns_what_fits() {
    assert_eq!(subagent_cols::cols_width(3), 28);
    assert_eq!(subagent_cols::cols_width(2), 17);
    assert_eq!(subagent_cols::cols_width(1), 6);
    assert_eq!(subagent_cols::keep_for(52), 3);
    assert_eq!(subagent_cols::keep_for(51), 2);
    assert_eq!(subagent_cols::keep_for(41), 2);
    assert_eq!(subagent_cols::keep_for(40), 1);
    assert_eq!(subagent_cols::keep_for(29), 0);
}

#[test]
fn right_at_ends_the_columns_at_the_edge_and_cuts_the_left_first() {
    let t = rich();
    let left = vec![Span::new("   ├ Agent  Security review", t.fg2)];
    let right = vec![Span::new("12s", t.fg3)];
    let out = subagent_cols::right_at(&left, &right, 40);
    assert_eq!(string_width(&text(&out)), 40);
    assert!(text(&out).starts_with("   ├ Agent  Security review"));
    assert!(text(&out).ends_with("  12s"));
    let narrow = subagent_cols::right_at(&left, &right, 20);
    assert_eq!(string_width(&text(&narrow)), 20);
    assert!(text(&narrow).ends_with("  12s"), "{}", text(&narrow));
    let cells = Cells {
        time: vec![Span::new("12s", t.fg3)],
        tokens: vec![Span::new("3.1k", t.fg3)],
        speed: Vec::new(),
    };
    assert_eq!(
        text(&subagent_cols::columns(&cells, 2)),
        "   12s       3.1k"
    );
}

#[test]
fn a_todo_step_s_mark_says_where_it_is() {
    let t = rich();
    assert_eq!(todo_step::status("completed"), Step::Done);
    assert_eq!(todo_step::status("in_progress"), Step::Now);
    assert_eq!(todo_step::status("pending"), Step::Todo);
    assert_eq!(
        text(&todo_step::step(&t, Step::Done, "Plan", false)),
        "✓ Plan"
    );
    assert_eq!(
        text(&todo_step::step(&t, Step::Todo, "Ship", false)),
        "· Ship"
    );
}

/// 远端的命令在跑：行尾 `@dev-a` 与用时之间只有一个 ` · `（修的问题：`· 9s` 自己带点，以前写成 `@dev-a · · 9s`）。
#[test]
fn a_remote_command_has_one_dot_between_its_machine_and_its_time() {
    let input = json!({"command": "touch remote.txt"});
    let mut c = done("t1", "Bash", &input);
    c.status = Status::Running;
    c.ended = false;
    if let Some(f) = c.facts.as_mut() {
        f.started_at = 1_000;
    }
    let w = here();
    let cells = |_: &Call<'_>, _: Option<i64>| Cells::default();
    let machine = |_: &Call<'_>| Some(("dev-a".to_owned(), false));
    let ctx = ToolCtx {
        theme: rich(),
        where_: &w,
        tasks: &|_| None,
        now: 1_000,
        now_ms: 10_000.0,
        details: false,
        todos: &no_todos,
        steps: &|_| None,
        cells: &cells,
        machine: &machine,
        pointed: None,
    };
    let el = tool_row::call(&ctx, &c, &ToolClock::default(), None);
    let row = lines(&el, 80).remove(0);
    assert!(row.ends_with("touch remote.txt  @dev-a · 9s"), "{row}");
}
