// 改写自 apps/zcode-host/crates/tui/src/ui/cells/tools_tests.rs（feat/rs 79b6691c55）：行换成 `view::Row`；
// diff 从工具输入算，不带行号（patch.rs）。
use ratatui::style::Color;
use serde_json::json;

use crate::ui::cells::tests::{b, ctx, done, text, tool};
use crate::ui::cells::{Ctx, build, render};
use crate::ui::i18n::{Lang, ZH};
use crate::ui::palette::{ColorLevel, Palette};
use crate::ui::text::plain;
use crate::view::{Body, Row, ToolRow, ToolStatus};

fn with(mut row: Row, change: impl FnOnce(&mut ToolRow)) -> Row {
    if let Body::Tool(tool) = &mut row.body {
        change(tool);
    }
    row
}

#[test]
fn each_tool_is_one_block_named_with_its_arguments() {
    let rows = [
        done(
            1,
            "Read",
            json!({"file_path": "/w/src/a.rs"}),
            "1\tfoo\n2\tbar\n3\tbaz",
        ),
        done(
            2,
            "Grep",
            json!({"pattern": "x", "path": "/w/src"}),
            "Found 2 files\n/w/a\n/w/b",
        ),
        done(3, "Glob", json!({"pattern": "*.md"}), "No files found"),
        done(
            4,
            "Grep",
            json!({"pattern": "y", "output_mode": "content"}),
            "a:1:y\nb:2:y",
        ),
    ];
    // 每个工具单独成块，不合并。
    assert_eq!(build(&rows, false, &[]).len(), 4);
    assert_eq!(
        text(&rows, false),
        [
            b("Read(src/a.rs)"),
            "  ⎿  Read 3 lines".into(),
            b("Grep(pattern: \"x\", path: \"src\")"),
            "  ⎿  Found 2 files".into(),
            b("Glob(pattern: \"*.md\")"),
            "  ⎿  Found 0 files".into(),
            b("Grep(pattern: \"y\")"),
            "  ⎿  Found 2 lines".into(),
        ]
    );
}

#[test]
fn commands_show_the_first_lines_of_their_output() {
    let output: String = (1..=10).map(|n| format!("line {n}\n")).collect();
    let row = done(
        1,
        "Bash",
        json!({"command": "cargo test\n--release"}),
        &output,
    );
    assert_eq!(
        text(std::slice::from_ref(&row), false),
        [
            b("Bash(cargo test …)"),
            "  ⎿  line 1".into(),
            "     line 2".into(),
            "     line 3".into(),
            "     … +7 lines".into(),
        ]
    );
    // 详细模式：完整的命令（续行缩进 2 列）与全部输出。
    let expanded = text(&[row], true);
    assert_eq!(expanded[0], b("Bash(cargo test"));
    assert_eq!(expanded[1], "  --release)");
    assert_eq!(expanded.len(), 12);
    let failed = with(
        tool(3, "Bash", ToolStatus::Error, json!({"command": "false"})),
        |tool| tool.output = "exit code 1".into(),
    );
    assert_eq!(
        text(&[failed], false),
        [b("Bash(false)"), "  ⎿  exit code 1".into()]
    );
    let quiet = done(4, "Bash", json!({"command": "true"}), "");
    assert_eq!(
        text(&[quiet], false),
        [b("Bash(true)"), "  ⎿  (No output)".into()]
    );
    let waiting = tool(
        5,
        "Bash",
        ToolStatus::Running,
        json!({"command": "sleep 9"}),
    );
    assert_eq!(
        text(std::slice::from_ref(&waiting), false),
        [b("Bash(sleep 9)"), "  ⎿  Running…".into()]
    );
    let streaming = with(waiting, |tool| tool.preview = "p1\np2\np3\np4\np5".into());
    assert_eq!(
        text(&[streaming], false),
        [
            b("Bash(sleep 9)"),
            "  ⎿  p3".into(),
            "     p4".into(),
            "     p5".into()
        ]
    );
}

fn edit(old: &str, new: &str) -> Row {
    done(
        1,
        "Edit",
        json!({"file_path": "/w/a.txt", "old_string": old, "new_string": new}),
        "ok",
    )
}

#[test]
fn edits_show_counts_and_a_diff_from_the_input() {
    assert_eq!(
        text(
            &[edit("context\nbefore\ntail", "context\nafter\ntail")],
            false
        ),
        [
            b("Edit(a.txt)"),
            "  ⎿  Added 1 line, removed 1 line".into(),
            "       context".into(),
            "     - before".into(),
            "     + after".into(),
            "       tail".into(),
        ]
    );
    // 新文件：Wrote；只有新增时不写 removed。
    let created = with(
        done(
            2,
            "Write",
            json!({"file_path": "/w/b.txt", "content": "x\ny\n"}),
            "ok",
        ),
        |tool| tool.display = json!({"type": "create", "filePath": "/w/b.txt"}),
    );
    assert_eq!(
        text(&[created], false),
        [
            b("Write(b.txt)"),
            "  ⎿  Wrote 2 lines to b.txt".into(),
            "     + x".into(),
            "     + y".into(),
        ]
    );
    // 紧凑模式最多 20 行 diff。
    let many: String = (0..25).map(|n| format!("l{n}\n")).collect();
    let out = text(&[edit("", &many)], false);
    assert_eq!(out[1], "  ⎿  Added 25 lines");
    assert_eq!(out.len(), 2 + 20 + 1);
    assert_eq!(out.last().unwrap(), "     … +5 lines");
    assert_eq!(text(&[edit("", &many)], true).len(), 2 + 25);
}

#[test]
fn paired_lines_highlight_the_changed_words() {
    let palette = Palette::new(ColorLevel::TrueColor, Some((0, 0, 0)));
    let ctx = ctx(&palette, false);
    let row = edit("let a = 1;", "let a = 2;");
    let lines = render(&build(std::slice::from_ref(&row), false, &[])[0], &ctx);
    let added = lines
        .iter()
        .find(|l| plain(l).ends_with("+ let a = 2;"))
        .unwrap();
    let line_bg = added.style.bg;
    let changed = added.spans.iter().find(|s| s.content == "2").unwrap();
    assert!(changed.style.bg.is_some() && changed.style.bg != line_bg);
    let same = added
        .spans
        .iter()
        .find(|s| s.content.contains("let"))
        .unwrap();
    assert_eq!(
        same.style.bg, None,
        "unchanged words keep the line background"
    );
    // 改动超过一行的 40%：不按词高亮。
    let row = edit("abc", "xyz");
    let lines = render(&build(std::slice::from_ref(&row), false, &[])[0], &ctx);
    let added = lines.iter().find(|l| plain(l).ends_with("+ xyz")).unwrap();
    assert!(
        added
            .spans
            .iter()
            .all(|s| s.style.bg.is_none() || s.style.bg == Some(Color::Reset))
    );
}

#[test]
fn waiting_interrupted_and_failed_tools() {
    let waiting = tool(1, "Bash", ToolStatus::Approval, json!({"command": "rm x"}));
    assert_eq!(
        text(&[waiting], false),
        [b("Bash(rm x)"), "  ⎿  Waiting for permission…".into()]
    );
    let cancelled = tool(
        2,
        "Edit",
        ToolStatus::Cancelled,
        json!({"file_path": "/w/a.rs"}),
    );
    assert_eq!(
        text(&[cancelled], false),
        [
            b("Edit(a.rs)"),
            "  ⎿  Interrupted · What should ZCode do instead?".into()
        ]
    );
    let failed = with(
        tool(3, "Read", ToolStatus::Error, json!({"file_path": "/w/x"})),
        |tool| tool.output = "File does not exist.".into(),
    );
    assert_eq!(
        text(&[failed], false),
        [b("Read(x)"), "  ⎿  File does not exist.".into()]
    );
}

#[test]
fn questions_and_plans_show_their_outcome() {
    let question = done(
        1,
        "AskUserQuestion",
        json!({"questions": [{"question": "Pick?"}, {"question": "Skip?"}]}),
        "User has answered your questions: \"Pick?\"=\"B\". You can now continue.",
    );
    assert_eq!(
        text(&[question], false),
        [b("AskUserQuestion"), "  ⎿  · Pick? → B".into()]
    );
    let approved = done(
        2,
        "ExitPlanMode",
        json!({"plan": "1. do it"}),
        "User has approved your plan.",
    );
    assert_eq!(
        text(std::slice::from_ref(&approved), false),
        [b("User approved ZCode's plan")]
    );
    assert!(
        text(&[approved], true)
            .iter()
            .any(|l| l.contains("1. do it"))
    );
    let rejected = tool(
        3,
        "ExitPlanMode",
        ToolStatus::Cancelled,
        json!({"plan": "x"}),
    );
    assert_eq!(text(&[rejected], false), [b("User rejected ZCode's plan")]);
}

#[test]
fn the_detailed_view_wraps_long_commands_and_output() {
    let long = "x".repeat(130);
    let row = done(1, "Bash", json!({"command": format!("echo {long}")}), &long);
    let compact = text(std::slice::from_ref(&row), false);
    assert_eq!(compact.len(), 2, "compact clips: {compact:?}");
    let detailed = text(&[row], true);
    assert!(detailed.len() >= 6, "{detailed:?}");
    assert!(detailed.iter().any(|l| l.starts_with("  ⎿  x")));
    assert!(
        detailed
            .iter()
            .all(|l| unicode_width::UnicodeWidthStr::width(l.as_str()) <= 60)
    );
}

#[test]
fn chinese_details() {
    let palette = Palette::new(ColorLevel::Ansi16, None);
    let ctx = Ctx {
        lang: Lang::Zh,
        strings: &ZH,
        ..ctx(&palette, false)
    };
    let row = done(1, "Read", json!({"file_path": "/w/a.rs"}), "1\ta\n2\tb");
    let lines: Vec<String> = render(&build(std::slice::from_ref(&row), false, &[])[0], &ctx)
        .iter()
        .map(plain)
        .collect();
    assert_eq!(lines[1], "  ⎿  读取了 2 行");
}

/// R5-1：输入不是合法 JSON 的调用，参数位置是固定的说明。
#[test]
fn an_unparsed_input_shows_its_byte_count() {
    let rows = [done(
        1,
        "Read",
        json!({"__unparsedToolInput": {"raw": "{\"file_path\": \"/w/x", "len": 18}}),
        "<tool_use_error>InputValidationError: Read was called with input that could not be parsed as JSON.</tool_use_error>",
    )];
    assert_eq!(
        text(&rows, false)[0],
        b("Read(input JSON failed to parse \u{2014} 18 bytes)")
    );
}

/// WaitForMcpServers（ext.md §3.8，修复线 F36）：显示名与参数文字；成功不画结果，失败照常画错误。
#[test]
fn waiting_for_mcp_servers_has_its_own_label() {
    let palette = Palette::new(ColorLevel::Ansi16, None);
    let parts = |row: &Row| {
        let Body::Tool(tool) = &row.body else {
            panic!("not a tool row");
        };
        super::head_parts(tool, &ctx(&palette, false))
    };
    let all = done(
        1,
        "WaitForMcpServers",
        json!({}),
        "ready: true\nConnected (their tools are now available — call them directly): slow",
    );
    assert_eq!(
        parts(&all),
        (
            "MCP Wait For Servers".to_owned(),
            Some("Wait for pending MCP servers to connect".to_owned()),
            ""
        )
    );
    // 成功：只有一行，下面没有结果。
    assert_eq!(text(&[all], false).len(), 1);
    let named = with(
        tool(
            2,
            "WaitForMcpServers",
            ToolStatus::Error,
            json!({"servers": ["slow", "db"]}),
        ),
        |tool| tool.output = "ready: false\nStill connecting".into(),
    );
    assert_eq!(
        parts(&named).1.as_deref(),
        Some("Wait for MCP servers to connect: slow, db")
    );
    let lines = text(&[named], false);
    assert_eq!(&lines[1..], ["  ⎿  ready: false", "     Still connecting"]);
}

/// 远端执行 R4（remote-exec.md §21.6）：远端的调用在行尾接 `  @机器`（强调色），`machine` 不进参数摘要；`local` 不标。
#[test]
fn remote_calls_end_with_their_machine() {
    let rows = [
        done(
            1,
            "Grep",
            json!({"pattern": "x", "machine": "dev-a"}),
            "Found 1 file\n/srv/a",
        ),
        done(
            2,
            "Glob",
            json!({"pattern": "*.md", "machine": "local"}),
            "No files found",
        ),
    ];
    assert_eq!(
        text(&rows, false),
        [
            format!("{}  @dev-a", b("Grep(pattern: \"x\")")),
            "  ⎿  Found 1 file".into(),
            b("Glob(pattern: \"*.md\")"),
            "  ⎿  Found 0 files".into(),
        ]
    );
    let palette = Palette::new(ColorLevel::TrueColor, Some((0, 0, 0)));
    let lines = render(&build(&rows, false, &[])[0], &ctx(&palette, false));
    let tag = lines[0].spans.last().unwrap();
    assert_eq!(tag.content, "@dev-a");
    assert_eq!(tag.style.fg, Some(palette.accent));
}

/// 远端 MCP（R5，remote-exec.md §22.8）：绑定到远端机器的 MCP 服务器的工具行在行尾接 `  @机器`（强调色），不并进工作段；
/// 本机的服务器不标、照常并进。
#[test]
fn tools_of_a_server_on_a_remote_machine_end_with_its_machine() {
    let rows = [
        done(
            1,
            "mcp__browser-a__navigate",
            json!({"url": "https://example.com"}),
            "ok",
        ),
        done(2, "mcp__docs__search", json!({"q": "x"}), "ok"),
    ];
    let marks = [("browser-a".to_owned(), "dev-a".to_owned())];
    let palette = Palette::new(ColorLevel::TrueColor, Some((0, 0, 0)));
    let base = ctx(&palette, false);
    let ctx = Ctx {
        width: 80,
        mcp_machines: &marks,
        machines: &[],
        ..base
    };
    let cells = build(&rows, true, &marks);
    // 远端的调用单独一块；本机的 MCP 调用照常进工作段。
    assert_eq!(cells.len(), 2);
    let head = render(&cells[0], &ctx);
    assert_eq!(
        plain(&head[0]),
        "⏺ browser-a.navigate(url: \"https://example.com\") (MCP)  @dev-a"
    );
    let tag = head[0].spans.last().unwrap();
    assert_eq!(tag.content, "@dev-a");
    assert_eq!(tag.style.fg, Some(palette.accent));
    let local = render(&build(&rows[1..], false, &marks)[0], &ctx);
    assert!(!plain(&local[0]).contains('@'), "{}", plain(&local[0]));
}

/// 机器服务的快照里的一台（F98 的工具行测试用）。
fn machine(status: crate::machines::MachineStatus) -> crate::machines::MachineRow {
    crate::machines::MachineRow {
        name: "dev-a".into(),
        ssh: "dev-a".into(),
        working_directory: Some("/srv/proj".into()),
        status,
        latency_ms: None,
        executor: None,
    }
}

fn lines_with(rows: &[Row], machines: &[crate::machines::MachineRow], lang: Lang) -> Vec<String> {
    let palette = Palette::new(ColorLevel::Ansi16, None);
    let base = ctx(&palette, false);
    let ctx = Ctx {
        machines,
        lang,
        strings: if lang == Lang::Zh { &ZH } else { base.strings },
        ..base
    };
    build(rows, false, &[])
        .iter()
        .flat_map(|cell| render(cell, &ctx))
        .map(|line| plain(&line))
        .collect()
}

/// F98（remote-exec.md §25.4.1，Docker 试用 L3）：远端的调用还没结束、那台机器在重连时，细节行写明在等它重连（Bash 换掉
/// “运行中…”，别的工具加这一行）；机器就绪时照旧。旧代码只写“运行中…”。
#[test]
fn a_remote_call_waiting_for_its_machine_says_so() {
    let rows = [
        tool(
            1,
            "Bash",
            ToolStatus::Running,
            json!({"command": "uptime", "machine": "dev-a"}),
        ),
        tool(
            2,
            "Read",
            ToolStatus::Pending,
            json!({"file_path": "/srv/proj/a.txt", "machine": "dev-a"}),
        ),
    ];
    let reconnecting = [machine(crate::machines::MachineStatus::Reconnecting {
        attempt: 6,
        retry_at_ms: Some(11_000),
        reason: "Connection refused".into(),
    })];
    assert_eq!(
        lines_with(&rows, &reconnecting, Lang::En),
        [
            format!("{}  @dev-a", b("Bash(uptime)")),
            "  ⎿  Waiting for dev-a to reconnect…".into(),
            format!("{}  @dev-a", b("Read(/srv/proj/a.txt)")),
            "  ⎿  Waiting for dev-a to reconnect…".into(),
        ]
    );
    assert_eq!(
        lines_with(&rows[..1], &reconnecting, Lang::Zh)[1],
        "  ⎿  等待 dev-a 重连…"
    );
    let ready = [machine(crate::machines::MachineStatus::Ready)];
    assert_eq!(
        lines_with(&rows, &ready, Lang::En),
        [
            format!("{}  @dev-a", b("Bash(uptime)")),
            "  ⎿  Running…".into(),
            format!("{}  @dev-a", b("Read(/srv/proj/a.txt)")),
        ]
    );
}

/// F98（remote-exec.md §25.4.5，Docker 试用 L10）：TaskStop 停的是远端的命令时，结果的 `display.machine` 让工具行接
/// `@dev-a`；本机的不标。旧代码的 TaskStop 行没有机器。
#[test]
fn a_stopped_remote_task_ends_with_its_machine() {
    let stopped = |display| {
        with(
            done(1, "TaskStop", json!({"task_id": "bux6rbkue"}), "{}"),
            |tool| tool.display = display,
        )
    };
    assert_eq!(
        text(&[stopped(json!({"machine": "dev-a"}))], false)[0],
        format!("{}  @dev-a", b("TaskStop(task_id: \"bux6rbkue\")"))
    );
    assert_eq!(
        text(&[stopped(serde_json::Value::Null)], false)[0],
        b("TaskStop(task_id: \"bux6rbkue\")")
    );
}

/// F98（remote-exec.md §25.4.6，Docker 试用 L12）：远端调用的路径原样显示，不按本机工作区（`/w`）缩写；本机的照旧写相对
/// 路径。旧代码把 dev-a 上恰好在 `/w` 下的路径写成相对的。
#[test]
fn remote_paths_are_not_shortened_by_the_local_workspace() {
    let rows = [
        done(
            1,
            "Read",
            json!({"file_path": "/w/same.txt", "machine": "dev-a"}),
            "1\tremote",
        ),
        done(2, "Read", json!({"file_path": "/w/same.txt"}), "1\tlocal"),
        with(
            done(
                3,
                "Write",
                json!({"file_path": "/w/notes/plan.md", "content": "a\nb\n", "machine": "dev-a"}),
                "",
            ),
            |tool| tool.display = json!({"type": "create"}),
        ),
    ];
    let lines = text(&rows, false);
    assert_eq!(lines[0], format!("{}  @dev-a", b("Read(/w/same.txt)")));
    assert!(lines.contains(&b("Read(same.txt)")), "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|line| line.ends_with("to /w/notes/plan.md")),
        "{lines:?}"
    );
}
