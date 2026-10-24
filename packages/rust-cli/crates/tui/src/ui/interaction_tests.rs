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
        machine: None,
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
        height: 40,
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
    // 工作线 F76：焦点行同提问视图的 `❯`（以前是选择列表组件的 `›` 与底色条）。
    assert!(shown.iter().any(|l| l.contains("❯ 1. Yes")));
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
        machine: None,
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

fn bash_ask(command: &str, suggestions: &[&str]) -> InteractionRequest {
    InteractionRequest::Permission {
        machine: None,
        tool_use: ToolUseId::new("tu5"),
        tool: "Bash".into(),
        input: json!({ "command": command }),
        message: "This command requires approval".into(),
        suggestions: suggestions
            .iter()
            .map(|content| rust_cli_kernel_api::permission::PermissionRule {
                tool: "Bash".into(),
                content: Some((*content).into()),
            })
            .collect(),
        asked_by: None,
    }
}

fn suggested() -> InteractionRequest {
    bash_ask("npm test", &["npm test *"])
}

fn shown(request: &InteractionRequest, lang: Lang) -> Vec<String> {
    let palette = Palette::new(ColorLevel::Ansi16, None);
    let view = View {
        palette: &palette,
        strings: &EN,
        lang,
        width: 120,
        height: 40,
        now_ms: 0,
    };
    let mut ui = Interaction::new(InteractionId::new("i1"), request, Instant::now());
    ui.workspace = "/w".to_owned();
    ui.lines(request, &view).0.iter().map(plain).collect()
}

fn remember(content: &str) -> Value {
    json!({"decision": "allow", "remember": [{"tool": "Bash", "content": content}]})
}

/// 判决链给了建议规则：多一行"不再询问"；Bash 的这一行是种子（工作线 F72，shell-permissions.md §4.10）。
#[test]
fn a_suggested_rule_adds_the_dont_ask_again_row() {
    assert_eq!(
        answer(&suggested(), &[KeyCode::Char('2')]),
        Some(remember("npm test *"))
    );
    assert_eq!(
        answer(&suggested(), &[KeyCode::Down, KeyCode::Enter]),
        Some(remember("npm test *"))
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
    let lines = shown(&suggested(), Lang::En);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("2. Yes, and don\u{2019}t ask again for: npm test *")),
        "{lines:?}"
    );
    assert!(lines.iter().any(|l| l.contains("3. No")), "{lines:?}");
    let lines = shown(&suggested(), Lang::Zh);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("2. 是，以后不再询问：npm test *")),
        "{lines:?}"
    );
}

/// 种子按询问框的规则算：第二个词不像子命令时只取命令名，不用判决链的精确规则。
#[test]
fn the_seed_is_not_the_exact_suggestion() {
    let request = bash_ask("git -C /repo status", &["git -C /repo status"]);
    assert_eq!(
        answer(&request, &[KeyCode::Down, KeyCode::Enter]),
        Some(remember("git *"))
    );
    let request = bash_ask("docker compose up -d", &["docker compose *"]);
    assert_eq!(
        answer(&request, &[KeyCode::Char('2')]),
        Some(remember("docker compose up *"))
    );
}

/// 焦点在种子这一行时键入的字改种子（数字也是字），退格删字；`*` 是整个 Bash，空的只允许这一次。
#[test]
fn the_seed_row_is_editable() {
    let request = bash_ask("npm test", &["npm test *"]);
    let mut keys = vec![KeyCode::Down];
    keys.extend([KeyCode::Backspace; 6]);
    keys.extend(chars("run 2 *"));
    keys.push(KeyCode::Enter);
    assert_eq!(answer(&request, &keys), Some(remember("npm run 2 *")));
    let mut keys = vec![KeyCode::Down];
    keys.extend([KeyCode::Backspace; 10]);
    keys.push(KeyCode::Char('*'));
    keys.push(KeyCode::Enter);
    assert_eq!(
        answer(&request, &keys),
        Some(json!({"decision": "allow", "remember": [{"tool": "Bash"}]}))
    );
    let mut keys = vec![KeyCode::Down];
    keys.extend([KeyCode::Backspace; 10]);
    keys.push(KeyCode::Enter);
    assert_eq!(answer(&request, &keys), Some(json!({"decision": "allow"})));
}

/// 复合命令有多条建议：没有种子，这一行列出全部建议规则与工作目录，选它记住全部。
#[test]
fn several_suggestions_are_listed() {
    let request = bash_ask(
        "mytool a && othertool b c",
        &["mytool a *", "othertool b *"],
    );
    let lines = shown(&request, Lang::En);
    assert!(
        lines.iter().any(|l| l
            .contains("2. Yes, and don't ask again for mytool a and othertool b commands in /w")),
        "{lines:?}"
    );
    assert_eq!(
        answer(&request, &[KeyCode::Char('2')]),
        Some(json!({"decision": "allow", "remember": [
            {"tool": "Bash", "content": "mytool a *"},
            {"tool": "Bash", "content": "othertool b *"},
        ]}))
    );
}

/// 旧会话的询问（agents.md §4.1.2，工作线 F72）：标题后标出会话与子代理；会话没有标题时写“之前的会话”。
#[test]
fn asks_of_closed_sessions_name_the_session() {
    let request = InteractionRequest::Permission {
        machine: None,
        tool_use: ToolUseId::new("tu9"),
        tool: "Bash".into(),
        input: json!({"command": "mytool build"}),
        message: "This command requires approval".into(),
        suggestions: Vec::new(),
        asked_by: Some("Explore".into()),
    };
    let palette = Palette::new(ColorLevel::Ansi16, None);
    let title = |origin: &str, lang: Lang| {
        let view = View {
            palette: &palette,
            strings: &EN,
            lang,
            width: 120,
            height: 40,
            now_ms: 0,
        };
        let mut ui = Interaction::new(InteractionId::new("i1"), &request, Instant::now());
        ui.origin = Some(origin.to_owned());
        plain(&ui.lines(&request, &view).0[1])
    };
    assert!(
        title("Fix the build", Lang::En)
            .ends_with("Bash command · from the Explore agent in session \"Fix the build\""),
        "{}",
        title("Fix the build", Lang::En)
    );
    assert!(title("", Lang::En).ends_with("· from the Explore agent in an earlier session"));
    assert!(
        title("Fix the build", Lang::Zh).ends_with("· 来自会话「Fix the build」的 Explore 子代理")
    );
    assert!(title(" ", Lang::Zh).ends_with("· 来自之前的会话的 Explore 子代理"));
}
