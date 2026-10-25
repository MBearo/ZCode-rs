// 改写自 apps/zcode-host/crates/tui/src/ui/cells/group_tests.rs（feat/rs 79b6691c55）：行换成 `view::Row`。
use std::time::Duration;

use ratatui::style::Modifier;
use serde_json::json;

use crate::ui::cells::tests::{b, done, row};
use crate::ui::cells::*;
use crate::ui::i18n::{EN, Lang, ZH};
use crate::ui::palette::{ColorLevel, Palette};
use crate::ui::text::plain;
use crate::view::{Body, Row, ToolStatus, TurnState};

fn ctx_at<'a>(palette: &'a Palette, lang: Lang, expanded: bool, now_ms: u64) -> Ctx<'a> {
    Ctx {
        palette,
        lang,
        strings: if lang == Lang::Zh { &ZH } else { &EN },
        width: 160,
        expanded,
        // 圆点亮着的那一半周期。
        elapsed: Duration::ZERO,
        now_ms,
        workspace: "/w",
        workspace_label: "~/w",
        version: "1.0.0",
        mcp_machines: &[],
        machines: &[],
        markdown: None,
    }
}

fn lines_of(rows: &[Row], lang: Lang, now_ms: u64) -> Vec<String> {
    let palette = Palette::new(ColorLevel::Ansi16, None);
    let ctx = ctx_at(&palette, lang, false, now_ms);
    build(rows, true, &[])
        .iter()
        .flat_map(|cell| render(cell, &ctx))
        .map(|line| plain(&line))
        .collect()
}

fn summary(rows: &[Row]) -> Vec<String> {
    lines_of(rows, Lang::En, 0)
}

fn turn(id: u64, state: TurnState) -> Row {
    row(id, Body::Turn(state))
}

fn reasoning(id: u64, text: &str, created: u64, duration_ms: Option<u64>) -> Row {
    Row {
        created_ms: created,
        ..row(
            id,
            Body::Reasoning {
                text: text.into(),
                streaming: false,
                duration_ms,
            },
        )
    }
}

fn read(id: u64, path: &str) -> Row {
    done(id, "Read", json!({"file_path": path}), "1\tx")
}

fn bash(id: u64, command: &str) -> Row {
    done(id, "Bash", json!({"command": command}), "ok")
}

fn text(id: u64, body: &str) -> Row {
    row(
        id,
        Body::Agent {
            text: body.into(),
            streaming: false,
        },
    )
}

fn mcp(id: u64, server: &str) -> Row {
    done(
        id,
        &format!("mcp__{server}__find"),
        json!({"q": "x"}),
        "hit",
    )
}

#[test]
fn reads_searches_and_commands_fold_into_one_summary_line() {
    let rows = [
        turn(1, TurnState::Success),
        row(2, Body::User { text: "go".into() }),
        reasoning(3, "plan", 1_000, Some(6_000)),
        read(4, "/w/a.rs"),
        read(5, "/w/a.rs"),
        read(6, "/w/b.rs"),
        done(7, "Grep", json!({"pattern": "x"}), "Found 1 file"),
        done(8, "Glob", json!({"pattern": "*.rs"}), "Found 1 file"),
        bash(10, "ls"),
        bash(11, "cargo test"),
        mcp(12, "search"),
        mcp(13, "search"),
        mcp(14, "docs"),
        // 并进来但不计数。
        done(15, "ToolSearch", json!({"query": "x"}), "found"),
        text(16, "done"),
    ];
    assert_eq!(
        summary(&rows),
        [
            "❯ go".to_owned(),
            "  Thought for 6s, searched for 2 patterns, read 2 files, \
             called search 2 times, called docs, ran 2 shell commands"
                .into(),
            b("done"),
        ]
    );
}

#[test]
fn text_other_tools_and_turns_end_a_group() {
    let rows = [
        turn(1, TurnState::Success),
        read(2, "/w/a.rs"),
        text(3, "middle"),
        read(4, "/w/a.rs"),
        done(
            5,
            "Edit",
            json!({"file_path": "/w/a.rs", "old_string": "a", "new_string": "b"}),
            "",
        ),
        bash(6, "ls"),
        turn(7, TurnState::Success),
        bash(8, "ls"),
    ];
    let shown = summary(&rows);
    assert_eq!(shown[0], "  Read 1 file");
    assert_eq!(shown[1], b("middle"));
    assert_eq!(shown[2], "  Read 1 file");
    assert!(shown[3].starts_with(&b("Edit(a.rs)")), "{shown:?}");
    // 回合边界：两个回合的命令不并在一起。
    let groups = shown
        .iter()
        .filter(|l| *l == "  Ran 1 shell command")
        .count();
    assert_eq!(groups, 2, "{shown:?}");
}

#[test]
fn empty_reasoning_does_not_end_a_group_and_failures_stay_in_it() {
    let mut failed = bash(4, "false");
    if let Body::Tool(tool) = &mut failed.body {
        tool.status = ToolStatus::Error;
    }
    let rows = [
        turn(1, TurnState::Success),
        read(2, "/w/a.rs"),
        reasoning(3, "  ", 0, None),
        failed,
        read(6, "/w/b.rs"),
    ];
    assert_eq!(summary(&rows), ["  Read 2 files, ran 1 shell command"]);
}

#[test]
fn chinese_wording() {
    let rows = [
        turn(1, TurnState::Success),
        reasoning(2, "plan", 1_000, Some(2_000)),
        done(3, "Grep", json!({"pattern": "x"}), "Found 1 file"),
        read(4, "/w/a.rs"),
        bash(8, "ls"),
    ];
    assert_eq!(
        lines_of(&rows, Lang::Zh, 0),
        ["  思考了 2s，搜索了 1 个模式，读取了 1 个文件，运行了 1 条命令"]
    );
}

#[test]
fn the_summary_is_dim_with_bold_numbers() {
    let rows = [
        turn(1, TurnState::Success),
        read(2, "/w/a.rs"),
        read(3, "/w/b.rs"),
    ];
    let palette = Palette::new(ColorLevel::Ansi16, None);
    let ctx = ctx_at(&palette, Lang::En, false, 0);
    let cells = build(&rows, true, &[]);
    let lines = render(&cells[0], &ctx);
    let spans = &lines[0].spans;
    assert!(
        spans
            .iter()
            .filter(|s| !s.content.trim().is_empty())
            .all(|s| s.style.add_modifier.contains(Modifier::DIM)),
        "{spans:?}"
    );
    let number = spans.iter().find(|s| s.content == "2").expect("count span");
    assert!(number.style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn thinking_time_prefers_the_duration_then_the_next_row_then_now() {
    assert_eq!(
        summary(&[
            turn(1, TurnState::Success),
            reasoning(2, "plan", 1_000, Some(2_500)),
            text(3, "x")
        ])[0],
        "  Thought for 3s"
    );
    // 没有用时：到下一行出现的时间；单段最多 10 分钟。
    let next = Row {
        created_ms: 1_000 + 25 * 60_000,
        ..text(3, "x")
    };
    assert_eq!(
        summary(&[
            turn(1, TurnState::Success),
            reasoning(2, "plan", 1_000, None),
            next
        ])[0],
        "  Thought for 10m 0s"
    );
    // 流式中：到当前时间，进行时。
    let streaming = Row {
        created_ms: 1_000,
        ..row(
            2,
            Body::Reasoning {
                text: "line one\nline two".into(),
                streaming: true,
                duration_ms: None,
            },
        )
    };
    let shown = lines_of(&[turn(1, TurnState::Running), streaming], Lang::En, 5_200);
    assert_eq!(
        shown,
        [
            b("Thinking for 5s…"),
            "  ⎿  line one".into(),
            "     line two".into(),
        ]
    );
}

/// 远端执行 R4（remote-exec.md §21.6，暂定）：带远端 `machine` 的调用不并进工作段，单独一块，行尾标出机器。
#[test]
fn remote_calls_stay_out_of_the_summary() {
    let rows = [
        turn(1, TurnState::Success),
        bash(2, "ls"),
        done(
            3,
            "Bash",
            json!({"command": "uname -a", "machine": "dev-a"}),
            "Linux",
        ),
        bash(4, "pwd"),
    ];
    let shown = summary(&rows);
    assert_eq!(shown[0], "  Ran 1 shell command");
    assert_eq!(shown[1], format!("{}  @dev-a", b("Bash(uname -a)")));
    assert!(shown.contains(&"  Ran 1 shell command".to_owned()));
    assert_eq!(
        shown
            .iter()
            .filter(|line| line.contains("Ran 1 shell command"))
            .count(),
        2,
        "{shown:?}"
    );
}
