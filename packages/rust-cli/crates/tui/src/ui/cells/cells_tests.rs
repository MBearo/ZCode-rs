// 改写自 apps/zcode-host/crates/tui/src/ui/cells/cells_tests.rs（feat/rs 79b6691c55）：行换成 `view::Row`。
use std::time::Duration;

use rust_cli_base::models::GLM_5;
use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::permission::PermissionMode;
use serde_json::{Value, json};

use super::*;
use crate::ui::i18n::{EN, Lang};
use crate::ui::palette::{ColorLevel, Palette};
use crate::ui::text::plain;
use crate::view::Marker;

pub(crate) fn ctx<'a>(palette: &'a Palette, expanded: bool) -> Ctx<'a> {
    Ctx {
        palette,
        lang: Lang::En,
        strings: &EN,
        width: 60,
        expanded,
        elapsed: Duration::ZERO,
        now_ms: 0,
        workspace: "/w",
        workspace_label: "~/w",
        version: "1.0.0",
        mcp_machines: &[],
        machines: &[],
        markdown: None,
    }
}

pub(crate) fn row(id: u64, body: Body) -> Row {
    Row {
        id,
        version: 0,
        created_ms: 0,
        body,
    }
}

/// 逐块排版（不并工作段，工作段见 `group_tests.rs`）。
pub(crate) fn text(rows: &[Row], expanded: bool) -> Vec<String> {
    let palette = Palette::new(ColorLevel::Ansi16, None);
    let ctx = ctx(&palette, expanded);
    build(rows, false, &[])
        .iter()
        .flat_map(|cell| render(cell, &ctx))
        .map(|line| plain(&line))
        .collect()
}

pub(crate) fn tool(id: u64, name: &str, status: ToolStatus, input: Value) -> Row {
    row(
        id,
        Body::Tool(Box::new(ToolRow {
            id: ToolUseId::new(format!("tu{id}")),
            name: name.into(),
            input,
            status,
            output: String::new(),
            preview: String::new(),
            display: Value::Null,
            activity: None,
        })),
    )
}

pub(crate) fn done(id: u64, name: &str, input: Value, output: &str) -> Row {
    let mut row = tool(id, name, ToolStatus::Success, input);
    if let Body::Tool(tool) = &mut row.body {
        tool.output = output.into();
    }
    row
}

pub(crate) fn b(rest: &str) -> String {
    format!("{BULLET} {rest}")
}

fn agent(text: &str, streaming: bool) -> Body {
    Body::Agent {
        text: text.into(),
        streaming,
    }
}

#[test]
fn messages_markers_and_tools() {
    let rows = [
        row(1, Body::Turn(TurnState::Success)),
        row(
            2,
            Body::User {
                text: "fix it".into(),
            },
        ),
        row(
            3,
            Body::Reasoning {
                text: "**Planning**\n\nsteps".into(),
                streaming: false,
                duration_ms: Some(1000),
            },
        ),
        done(
            4,
            "mcp__search__find_docs",
            json!({"query": "x"}),
            "Found docs",
        ),
        row(5, agent("All **set**", false)),
        row(6, Body::Marker(Marker::Compacted)),
        row(7, Body::Error("busy".into())),
    ];
    assert_eq!(
        text(&rows, false),
        [
            "❯ fix it".into(),
            "∴ Planning".into(),
            "  ".into(),
            "  steps".into(),
            b("search.find_docs(query: \"x\") (MCP)"),
            "  ⎿  Found docs".into(),
            b("All set"),
            "✻ Conversation compacted (ctrl+o for history)".into(),
            b("busy"),
        ]
    );
}

/// L18：插话被 UserPromptSubmit 阻塞时的提示，多行原样显示，续行缩进两格。
#[test]
fn system_notices_show_every_line() {
    let rows = [row(
        1,
        Body::Notice {
            level: rust_cli_kernel_api::session_api::NoticeLevel::Warning,
            text: "UserPromptSubmit operation blocked by hook:\nno\n\nOriginal prompt: also b"
                .into(),
        },
    )];
    assert_eq!(
        text(&rows, false),
        [
            b("UserPromptSubmit operation blocked by hook:"),
            "  no".into(),
            "  ".into(),
            "  Original prompt: also b".into(),
        ]
    );
}

#[test]
fn interrupted_turns_end_with_a_hint() {
    let rows = [
        row(1, Body::Turn(TurnState::Interrupted)),
        row(2, Body::User { text: "go".into() }),
    ];
    assert_eq!(
        text(&rows, false),
        ["❯ go", "  ⎿  Interrupted · What should ZCode do instead?"]
    );
}

#[test]
fn the_startup_header_names_the_model_and_directory() {
    let palette = Palette::new(ColorLevel::Ansi16, None);
    let lines = |mode, model| -> Vec<String> {
        header(mode, model, &ctx(&palette, false))
            .iter()
            .map(plain)
            .collect()
    };
    assert_eq!(
        lines(PermissionMode::Default, GLM_5),
        ["ZCode v1.0.0".to_owned(), format!("{GLM_5} · ~/w")]
    );
    assert_eq!(lines(PermissionMode::Default, ""), ["ZCode v1.0.0", "~/w"]);
    assert_eq!(
        lines(PermissionMode::BypassPermissions, "m")[2],
        "permissions: Full access"
    );
}

#[test]
fn live_cells_are_the_ones_still_changing() {
    let rows = [row(1, agent("x", true))];
    assert!(build(&rows, false, &[])[0].live());
    let rows = [row(1, agent("x", false))];
    assert!(!build(&rows, false, &[])[0].live());
}

#[test]
fn running_dots_blink_every_600_ms() {
    let palette = Palette::new(ColorLevel::Ansi16, None);
    let row = tool(1, "Bash", ToolStatus::Running, json!({"command": "x"}));
    let cell = &build(std::slice::from_ref(&row), false, &[])[0];
    let at = |ms| {
        let ctx = Ctx {
            elapsed: Duration::from_millis(ms),
            ..ctx(&palette, false)
        };
        plain(&render(cell, &ctx)[0])
    };
    assert!(at(0).starts_with(BULLET));
    assert!(at(700).starts_with("  Bash"), "{}", at(700));
    assert!(at(1300).starts_with(BULLET));
}

#[test]
fn a_streaming_reply_hides_its_unfinished_last_line() {
    assert_eq!(
        text(&[row(1, agent("line one\nline tw", true))], false),
        [b("line one")]
    );
    assert!(text(&[row(1, agent("no newline yet", true))], false).is_empty());
    // 回复结束后全部显示（markdown 的软换行把两行连成一段）。
    assert_eq!(
        text(&[row(1, agent("line one\nline two", false))], false),
        [b("line one line two")]
    );
}

/// 定时任务的触发行（tools-more.md §18.3）：英文原文（产品名换成 ZCode）与中文。
#[test]
fn fire_rows_use_the_fixed_text_in_both_languages() {
    assert_eq!(
        words::fire(Lang::En, "Oct 18 3:04pm", false, None),
        "Running scheduled task (Oct 18 3:04pm)"
    );
    assert_eq!(
        words::fire(Lang::En, "Oct 18 3:04pm", true, None),
        "ZCode resuming /loop wakeup (Oct 18 3:04pm)"
    );
    assert_eq!(
        words::fire(Lang::En, "Oct 18 3:04pm", true, Some((1, "Oct 18 2:44pm"))),
        "ZCode resuming /loop wakeup (Oct 18 3:04pm) \u{b7} 1 no-op tick since Oct 18 2:44pm"
    );
    assert_eq!(
        words::fire(Lang::En, "Oct 18 3:04pm", true, Some((3, "Oct 18 2:44pm"))),
        "ZCode resuming /loop wakeup (Oct 18 3:04pm) \u{b7} 3 no-op ticks since Oct 18 2:44pm"
    );
    assert_eq!(
        words::fire(Lang::Zh, "Oct 18 3:04pm", true, Some((2, "Oct 18 2:44pm"))),
        "ZCode 恢复 /loop 唤醒（Oct 18 3:04pm） \u{b7} 自 Oct 18 2:44pm 起连续 2 次无动作"
    );
    let fire = crate::view::Fire {
        at: "Oct 18 3:04pm".into(),
        kind: rust_cli_kernel_api::cron::CronFireKind::Task,
        fold: None,
    };
    assert_eq!(
        text(&[row(1, Body::Fire(Box::new(fire)))], false),
        ["✻ Running scheduled task (Oct 18 3:04pm)"]
    );
}

/// 到了阻塞线（compact.md §2.2）：错误行是“上下文满了”的提示；带压缩失败原因的接上 `/clear`；其余错误原样。
#[test]
fn the_blocking_line_error_reads_as_a_full_context() {
    assert_eq!(
        text(&[row(1, Body::Error("Prompt is too long".into()))], false).join(""),
        "⏺ Context limit reached · /compact or /clear to   continue"
    );
    assert_eq!(
        words::turn_error(
            Lang::En,
            "Prompt is too long \u{b7} automatic compaction failed: boom"
        ),
        "Prompt is too long \u{b7} automatic compaction failed: boom \u{b7} /clear to start fresh"
    );
    assert_eq!(
        words::turn_error(Lang::Zh, "Prompt is too long"),
        "上下文已满 \u{b7} 用 /compact 或 /clear 继续"
    );
    assert_eq!(words::turn_error(Lang::En, "busy"), "busy");
}
