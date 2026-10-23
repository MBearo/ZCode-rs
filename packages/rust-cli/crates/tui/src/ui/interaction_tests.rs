//! 交互的回答（docs/specs/rust-cli/tui.md §5.2）。
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::ids::{InteractionId, ToolUseId};
use serde_json::{Value, json};

use super::*;
use crate::ui::i18n::{EN, Lang};
use crate::ui::palette::{ColorLevel, Palette};
use crate::ui::text::plain;

fn permission() -> InteractionRequest {
    InteractionRequest::Permission {
        tool_use: ToolUseId::new("tu1"),
        tool: "Bash".into(),
        input: json!({"command": "rm -rf build", "description": "clean the build"}),
        message: "rm needs approval".into(),
        suggestions: Vec::new(),
        asked_by: None,
    }
}

fn question() -> InteractionRequest {
    InteractionRequest::Question {
        tool_use: ToolUseId::new("tu2"),
        questions: json!([{
            "question": "Which database?", "header": "DB", "multiSelect": false,
            "options": [{"label": "Postgres", "description": "server"}, {"label": "SQLite", "description": "file"}],
        }]),
    }
}

fn plan() -> InteractionRequest {
    InteractionRequest::PlanApproval {
        tool_use: ToolUseId::new("tu3"),
        plan: "1. Add tests\n2. Fix it".into(),
    }
}

/// 弹出后 200 ms 依次按键，返回第一个回答。
fn answer(request: &InteractionRequest, keys: &[KeyCode]) -> Option<Value> {
    let start = Instant::now();
    let mut ui = Interaction::new(InteractionId::new("i1"), request, start);
    let at = start + Duration::from_millis(200);
    keys.iter().find_map(|code| {
        ui.on_key(
            request,
            KeyEvent::new(*code, KeyModifiers::NONE),
            at,
            Typing::default(),
        )
    })
}

fn chars(text: &str) -> Vec<KeyCode> {
    text.chars().map(KeyCode::Char).collect()
}

#[test]
fn permission_answers_allow_deny_and_feedback() {
    assert_eq!(
        answer(&permission(), &[KeyCode::Enter]),
        Some(json!({"decision": "allow"}))
    );
    assert_eq!(
        answer(&permission(), &[KeyCode::Down, KeyCode::Enter]),
        Some(json!({"decision": "deny"}))
    );
    assert_eq!(
        answer(&permission(), &[KeyCode::Esc]),
        Some(json!({"decision": "deny"}))
    );
    let mut keys = vec![KeyCode::Tab];
    keys.extend(chars("use make clean"));
    keys.push(KeyCode::Enter);
    assert_eq!(
        answer(&permission(), &keys),
        Some(json!({"decision": "deny", "feedback": "use make clean"}))
    );
}

#[test]
fn keys_right_after_the_view_opens_are_ignored() {
    let request = permission();
    let start = Instant::now();
    let mut ui = Interaction::new(InteractionId::new("i1"), &request, start);
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(ui.on_key(&request, enter, start, Typing::default()), None);
    assert!(
        ui.on_key(
            &request,
            enter,
            start + Duration::from_millis(151),
            Typing::default()
        )
        .is_some()
    );
}

#[test]
fn questions_answer_with_labels_or_decline() {
    assert_eq!(
        answer(&question(), &[KeyCode::Char('2')]),
        Some(json!({"decision": "answered", "answers": {"Which database?": "SQLite"}}))
    );
    // Other 行：输入自己的回答。
    let mut keys = vec![KeyCode::Char('3')];
    keys.extend(chars("DuckDB"));
    keys.push(KeyCode::Enter);
    assert_eq!(
        answer(&question(), &keys),
        Some(json!({"decision": "answered", "answers": {"Which database?": "DuckDB"}}))
    );
    assert_eq!(
        answer(&question(), &[KeyCode::Esc]),
        Some(json!({"decision": "declined"}))
    );
}

/// 修复线 F20：批准只关掉 plan（回答里的模式是弹出时的权限模式，只作记录），第二行是继续计划。
#[test]
fn plan_approval_approves_or_keeps_planning() {
    assert_eq!(
        answer(&plan(), &[KeyCode::Enter]),
        Some(json!({"decision": "approve", "mode": "default"}))
    );
    assert_eq!(
        answer(&plan(), &[KeyCode::Char('1')]),
        Some(json!({"decision": "approve", "mode": "default"}))
    );
    let mut keys = vec![KeyCode::Char('2')];
    keys.extend(chars("smaller steps"));
    keys.push(KeyCode::Enter);
    assert_eq!(
        answer(&plan(), &keys),
        Some(json!({"decision": "keep_planning", "feedback": "smaller steps"}))
    );
    assert_eq!(
        answer(&plan(), &[KeyCode::Esc]),
        Some(json!({"decision": "keep_planning"}))
    );
}

#[test]
fn views_show_the_request() {
    let palette = Palette::new(ColorLevel::Ansi16, None);
    let view = View {
        palette: &palette,
        strings: &EN,
        lang: Lang::En,
        width: 60,
        now_ms: 0,
    };
    let text = |request: &InteractionRequest| -> Vec<String> {
        let ui = Interaction::new(InteractionId::new("i1"), request, Instant::now());
        ui.lines(request, &view).0.iter().map(plain).collect()
    };
    let shown = text(&permission());
    assert!(
        shown.iter().any(|l| l.contains("Bash command")),
        "{shown:?}"
    );
    assert!(shown.iter().any(|l| l.contains("│ rm -rf build")));
    assert!(shown.iter().any(|l| l.contains("rm needs approval")));
    assert!(shown.iter().any(|l| l.contains("› 1. Yes")));
    let shown = text(&plan());
    assert!(shown.iter().any(|l| l.contains("Ready to code?")));
    assert!(shown.iter().any(|l| l.contains("1. Yes, exit plan mode")));
    assert!(shown.iter().any(|l| l.contains("2. No, keep planning")));
    assert!(!shown.iter().any(|l| l.contains("auto-accept edits")));
    let shown = text(&question());
    assert!(shown.iter().any(|l| l.contains("Which database?")));
    assert!(shown.iter().any(|l| l.contains("2. SQLite")));
    // Edit 的审批带 diff 预览。
    let edit = InteractionRequest::Permission {
        tool_use: ToolUseId::new("tu4"),
        tool: "Edit".into(),
        input: json!({"file_path": "/w/a.rs", "old_string": "a", "new_string": "b"}),
        message: String::new(),
        suggestions: Vec::new(),
        asked_by: None,
    };
    let shown = text(&edit);
    assert!(shown.iter().any(|l| l.contains("- a")), "{shown:?}");
    assert!(shown.iter().any(|l| l.contains("+ b")));
    assert!(shown.iter().any(|l| l.contains("make this edit to a.rs")));
}

fn suggested() -> InteractionRequest {
    InteractionRequest::Permission {
        tool_use: ToolUseId::new("tu5"),
        tool: "Bash".into(),
        input: json!({"command": "npm test"}),
        message: "npm test needs approval".into(),
        suggestions: vec![rust_cli_kernel_api::permission::PermissionRule {
            tool: "Bash".into(),
            content: Some("npm test:*".into()),
        }],
        asked_by: None,
    }
}

/// 判决链给了建议规则：多一行"不再询问"，选它时把规则带回（Host TUI 的 allowAlways）。
#[test]
fn a_suggested_rule_adds_the_dont_ask_again_row() {
    let remember = json!({
        "decision": "allow",
        "remember": [{"tool": "Bash", "content": "npm test:*"}],
    });
    assert_eq!(
        answer(&suggested(), &[KeyCode::Char('2')]),
        Some(remember.clone())
    );
    assert_eq!(
        answer(&suggested(), &[KeyCode::Down, KeyCode::Enter]),
        Some(remember)
    );
    assert_eq!(
        answer(
            &suggested(),
            &[KeyCode::Down, KeyCode::Down, KeyCode::Enter]
        ),
        Some(json!({"decision": "deny"}))
    );
    assert_eq!(
        answer(&suggested(), &[KeyCode::Char('3')]),
        Some(json!({"decision": "deny"}))
    );
    let palette = Palette::new(ColorLevel::Ansi16, None);
    let view = View {
        palette: &palette,
        strings: &EN,
        lang: Lang::En,
        width: 60,
        now_ms: 0,
    };
    let request = suggested();
    let ui = Interaction::new(InteractionId::new("i1"), &request, Instant::now());
    let shown: Vec<String> = ui.lines(&request, &view).0.iter().map(plain).collect();
    assert!(
        shown
            .iter()
            .any(|l| l.contains("2. Yes, and don't ask again for this command")),
        "{shown:?}"
    );
    assert!(shown.iter().any(|l| l.contains("3. No")), "{shown:?}");
}
