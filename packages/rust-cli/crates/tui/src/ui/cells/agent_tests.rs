//! Agent 工具行的排版（tui-display.md §2.1）。
use rust_cli_kernel_api::ids::ToolUseId;
use serde_json::{Value, json};

use super::*;
use crate::ui::cells::tests::{b, done, text, tool};
use crate::view::{Activity, AgentActivity, Body, ToolStatus};

fn read(id: &str, status: ToolStatus) -> Activity {
    Activity::Tool {
        id: ToolUseId::new(id),
        name: "Read".into(),
        input: json!({"file_path": format!("/w/{id}.rs")}),
        status,
    }
}

fn with_activity(items: Vec<Activity>) -> crate::view::Row {
    let mut row = tool(
        1,
        "Agent",
        ToolStatus::Running,
        json!({"description": "Find callers", "prompt": "p", "subagent_type": "Explore"}),
    );
    if let Body::Tool(tool) = &mut row.body {
        tool.activity = Some(AgentActivity {
            items,
            notice: None,
        });
    }
    row
}

#[test]
fn numbers_and_durations_are_compact() {
    assert_eq!(compact_number(532), "532");
    assert_eq!(compact_number(1000), "1.0k");
    assert_eq!(compact_number(12_345), "12.3k");
    assert_eq!(compact_number(2_000_000), "2.0m");
    assert_eq!(duration(0), "0s");
    assert_eq!(duration(4_900), "4s");
    assert_eq!(duration(65_000), "1m 5s");
    assert_eq!(duration(3_723_000), "1h 2m 3s");
}

/// F58：tui 原来的 `duration` 用 f64 的 `round` 取秒，kernel-api 的 `format_duration` 用整数 `+ 500`；毫秒余数
/// 在 0–59999 上逐个相同，所以收拢后显示不变（`x / 1000.0` 在平分处是精确值，`round` 取远离零）。
#[test]
fn integer_and_float_second_rounding_agree() {
    for rest in 0..60_000_u64 {
        let float = ((rest as f64) / 1000.0).round() as u64;
        assert_eq!((rest + 500) / 1000, float, "{rest}");
    }
    assert_eq!(duration(119_500), "2m 0s");
    assert_eq!(duration(119_499), "1m 59s");
    assert_eq!(duration(86_399_500), "1d 0h 0m");
}

#[test]
fn running_agents_show_the_last_three_activities() {
    let lines = text(&[with_activity(Vec::new())], false);
    assert_eq!(
        lines,
        [b("Explore(Find callers)"), "  ⎿  Initializing…".to_owned()]
    );

    let items = vec![
        read("a", ToolStatus::Success),
        read("b", ToolStatus::Success),
        Activity::Text {
            message: 1,
            index: 0,
            text: "Now checking\nmore".into(),
        },
        read("c", ToolStatus::Success),
        read("d", ToolStatus::Running),
    ];
    let lines = text(&[with_activity(items.clone())], false);
    assert_eq!(
        lines,
        [
            b("Explore(Find callers)"),
            "  ⎿  Now checking".to_owned(),
            "     Read(c.rs)".to_owned(),
            "     Read(d.rs)".to_owned(),
            "     … +2 tool uses (ctrl+o to expand)".to_owned(),
        ]
    );
    // 详细视图列出全部活动。
    let lines = text(&[with_activity(items)], true);
    assert_eq!(lines.len(), 6, "{lines:#?}");
}

#[test]
fn finished_agents_show_done_or_backgrounded() {
    let input = json!({"description": "Find callers", "prompt": "p"});
    let handback = "Here is the report\n  found\nagentId: a1 (use SendMessage …)\n\
                    <usage>subagent_tokens: 12345\ntool_uses: 3\nduration_ms: 65000</usage>";
    let lines = text(&[done(1, "Agent", input.clone(), handback)], false);
    assert_eq!(
        lines,
        [
            b("Agent(Find callers)"),
            "  ⎿  Done (3 tool uses · 12.3k tokens · 1m 5s)".to_owned(),
            "  (ctrl+o to expand)".to_owned(),
        ]
    );
    let launched = "Async agent launched successfully. (This tool result is internal metadata)\n\
                    agentId: a1";
    let lines = text(&[done(2, "Agent", input.clone(), launched)], false);
    assert_eq!(
        lines,
        [
            b("Agent(Find callers)"),
            "  ⎿  Backgrounded agent (↓ to manage · ctrl+o to expand)".to_owned(),
        ]
    );
    // 没有尾注（one-shot agent）：按普通工具显示输出。
    let lines = text(&[done(3, "Agent", input, "just the report")], false);
    assert!(
        lines.iter().any(|line| line.contains("just the report")),
        "{lines:#?}"
    );
    let _ = Value::Null;
}

#[test]
fn a_retrying_agent_shows_the_retry_line_after_its_activity() {
    use rust_cli_kernel_api::model::{ModelError, ModelNotice};
    let mut row = with_activity(vec![read("a", ToolStatus::Success)]);
    if let Body::Tool(tool) = &mut row.body {
        tool.activity.as_mut().unwrap().notice = Some(crate::view::Notice {
            notice: ModelNotice::Retrying {
                attempt: 2,
                max_retries: 10,
                delay_ms: 8_000,
                error: ModelError::Overloaded {
                    message: "Overloaded".into(),
                },
            },
            at_ms: 0,
        });
    }
    let lines = text(&[row], false);
    assert_eq!(
        lines,
        [
            b("Explore(Find callers)"),
            "  ⎿  Read(a.rs)".to_owned(),
            "     ✻ API error · Retrying in 8s · attempt 2/10".to_owned(),
        ]
    );
}

#[test]
fn thinking_items_show_as_dim_italic_lines() {
    let thinking = |text: &str| Activity::Thinking {
        message: 1,
        index: 0,
        text: text.into(),
    };
    let lines = text(
        &[with_activity(vec![
            read("a", ToolStatus::Success),
            thinking("  "),
            thinking("Let me look\n  at the   callers"),
        ])],
        false,
    );
    // 推理文字的空白压成一个空格；空的推理项不显示。
    assert_eq!(
        lines,
        [
            b("Explore(Find callers)"),
            "  ⎿  Read(a.rs)".to_owned(),
            "     ∴ Let me look at the callers".to_owned(),
        ]
    );
}
