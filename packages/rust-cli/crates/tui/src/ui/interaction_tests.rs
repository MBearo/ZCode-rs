//! 请求的界面状态与按键（docs/specs/rust-cli/tui-kit.md §4.2「权限询问、提问、计划审批」，照 Puffin 的 `shell/asks.rs`）：
//! 打开后点亮之前不能回答、数字与 ⏎ 回答、打字选中 Deny 并带着字发出、提问逐页、计划的批准与继续计划、板上的字、槽的键、
//! 旧界面多出的可改前缀。
use std::time::{Duration, Instant};

use crossterm::event::KeyCode;
use rust_cli_kernel_api::ids::{InteractionId, ToolUseId};
use rust_cli_kernel_api::orchestration::{Answer, PermissionAnswer};
use rust_cli_kernel_api::permission::{PermissionMode, PermissionRule};
use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent};
use serde_json::{Value, json};

use super::*;
use crate::ui::app::{Action, App};
use crate::ui::i18n::Lang;
use crate::ui::lines::plain;

/// 点亮之后（[`ARM_TICKS`] 个 tick 之后）。
const ARMED: Duration = Duration::from_millis(ARM_TICKS as u64 * TICK_MS + 1);

fn app(lang: Lang) -> App {
    crate::ui::test_support::app_in(lang)
}

/// 请求 `id` 到达（视图收到 `InteractionAsked`，界面跟上）。
fn ask(app: &mut App, id: &str, request: InteractionRequest, now: Instant) {
    let seq = app.view.pending_all().len() as u64 + 1;
    app.view.apply(
        SequencedEvent {
            seq,
            revision: 0,
            event: KernelEvent::InteractionAsked {
                interaction: InteractionId::new(id),
                request,
            },
        },
        0,
    );
    app.sync_interaction(now);
}

fn bash(command: &str, suggestions: &[&str]) -> InteractionRequest {
    InteractionRequest::Permission {
        machine: Default::default(),
        tool_use: ToolUseId::new("tu1"),
        tool: "Bash".into(),
        input: json!({"command": command, "description": "clean the build"}),
        message: "This command requires approval".into(),
        suggestions: suggestions
            .iter()
            .map(|content| PermissionRule {
                tool: "Bash".into(),
                content: Some((*content).into()),
            })
            .collect(),
        rule_seed: None,
        asked_by: None,
    }
}

/// 带“不再询问”种子的 Bash 询问：种子由内核放进询问（算法在 policy），界面只显示、编辑。
fn seeded(command: &str, suggestions: &[&str], seed: &str) -> InteractionRequest {
    let mut request = bash(command, suggestions);
    if let InteractionRequest::Permission { rule_seed, .. } = &mut request {
        *rule_seed = Some(seed.to_owned());
    }
    request
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

/// 依次按键（都在 `at`），返回第一个回答。
fn press(app: &mut App, keys: &[KeyCode], at: Instant) -> Option<Action> {
    keys.iter()
        .find_map(|code| app.on_key(crate::ui::test_support::key(*code), at))
}

fn chars(text: &str) -> Vec<KeyCode> {
    text.chars().map(KeyCode::Char).collect()
}

/// 期望的回答写成 JSON（同前端交给内核之前的形状），比较时换成 `Answer`。
fn resolve(id: &str, answer: Value) -> Option<Action> {
    let answer: Answer = serde_json::from_value(answer).expect("an answer");
    Some(Action::Resolve {
        interaction: InteractionId::new(id),
        answer,
    })
}

/// 一个请求打开、点亮之后按 `keys` 的回答。
fn answered(request: InteractionRequest, keys: &[KeyCode]) -> Option<Action> {
    let start = Instant::now();
    let mut app = app(Lang::En);
    ask(&mut app, "i1", request, start);
    press(&mut app, keys, start + ARMED)
}

/// 板上的行与槽（纯文字）。
fn board(app: &App, at: Instant) -> (Vec<String>, String) {
    let (laid, _, _) = app.ask_laid(100, at, false).expect("an ask is open");
    let rows = laid
        .lines
        .iter()
        .map(|line| plain(line).trim_end().to_owned())
        .collect();
    let slot = laid.slot.iter().map(|span| &*span.text).collect::<String>();
    (rows, slot.trim().to_owned())
}

#[test]
fn permission_answers_by_digit_enter_or_typing() {
    let deny = json!({"decision": "deny"});
    assert_eq!(
        answered(bash("rm -rf build", &[]), &[KeyCode::Enter]),
        resolve("i1", json!({"decision": "allow"}))
    );
    // 没有建议规则：Allow once、Full access、Deny。
    assert_eq!(
        answered(bash("rm -rf build", &[]), &[KeyCode::Char('3')]),
        resolve("i1", deny.clone())
    );
    assert_eq!(
        answered(bash("rm -rf build", &[]), &[KeyCode::Up, KeyCode::Enter]),
        resolve("i1", deny)
    );
    // 打字：选中 Deny，⏎ 带着字发出。
    let mut keys = chars("use make clean");
    keys.push(KeyCode::Enter);
    assert_eq!(
        answered(bash("rm -rf build", &[]), &keys),
        resolve(
            "i1",
            json!({"decision": "deny", "feedback": "use make clean"})
        )
    );
    // 有建议规则：Always allow 记住它们。
    assert_eq!(
        answered(bash("npm test", &["npm test *"]), &[KeyCode::Char('2')]),
        resolve(
            "i1",
            json!({"decision": "allow", "remember": [{"tool": "Bash", "content": "npm test *"}]})
        )
    );
    // esc 停下回合，不是回答。
    assert_eq!(
        answered(bash("rm -rf build", &[]), &[KeyCode::Esc]),
        Some(Action::Interrupt)
    );
}

/// 打开后 [`ARM_TICKS`] 个 tick 内 ⏎ 与数字不起作用（Puffin `armed`）；回答发出后回答的键不起作用，直到它离开视图。
#[test]
fn answers_wait_to_arm_and_answer_once() {
    let start = Instant::now();
    let mut app = app(Lang::En);
    ask(&mut app, "i1", bash("rm -rf build", &[]), start);
    assert_eq!(press(&mut app, &[KeyCode::Enter], start), None);
    assert_eq!(press(&mut app, &[KeyCode::Char('1')], start), None);
    assert!(app.composer.area.text().is_empty());
    let at = start + ARMED;
    assert!(press(&mut app, &[KeyCode::Enter], at).is_some());
    assert_eq!(press(&mut app, &[KeyCode::Enter], at), None);
    assert_eq!(press(&mut app, &[KeyCode::Char('3')], at), None);
}

/// Full access 先经确认框；确认后允许这一次再切到完全访问（Puffin `YoloThen::Answer`）。
#[test]
fn full_access_asks_first() {
    let start = Instant::now();
    let mut app = app(Lang::En);
    ask(&mut app, "i1", bash("rm -rf build", &[]), start);
    let at = start + ARMED;
    assert_eq!(press(&mut app, &[KeyCode::Char('2')], at), None);
    assert!(matches!(
        app.overlay,
        Some(crate::ui::app::Overlay::YoloConfirm(_))
    ));
    let confirmed = press(
        &mut app,
        &[KeyCode::Up, KeyCode::Char(' '), KeyCode::Char('2')],
        at,
    );
    assert_eq!(
        confirmed,
        Some(Action::Many(vec![
            Action::Resolve {
                interaction: InteractionId::new("i1"),
                answer: Answer::Permission(PermissionAnswer::Allow {
                    remember: Vec::new(),
                }),
            },
            Action::SetMode(PermissionMode::BypassPermissions),
        ]))
    );
}

#[test]
fn questions_answer_by_pick_or_own_words() {
    assert_eq!(
        answered(question(), &[KeyCode::Char('2')]),
        resolve(
            "i1",
            json!({"decision": "answered", "answers": {"Which database?": "SQLite"}})
        )
    );
    assert_eq!(
        answered(question(), &[KeyCode::Down, KeyCode::Enter]),
        resolve(
            "i1",
            json!({"decision": "answered", "answers": {"Which database?": "SQLite"}})
        )
    );
    // 输入框里的字是这一页自己的回答。
    let mut keys = chars("DuckDB");
    keys.push(KeyCode::Enter);
    assert_eq!(
        answered(question(), &keys),
        resolve(
            "i1",
            json!({"decision": "answered", "answers": {"Which database?": "DuckDB"}})
        )
    );
}

/// 修复线 F20：批准只关掉 plan（回答里的模式是弹出时的权限模式）；打字是继续计划，带着字。
#[test]
fn plans_approve_or_keep_planning() {
    let approve = json!({"decision": "approve", "mode": "default"});
    assert_eq!(
        answered(plan(), &[KeyCode::Enter]),
        resolve("i1", approve.clone())
    );
    assert_eq!(
        answered(plan(), &[KeyCode::Char('1')]),
        resolve("i1", approve)
    );
    let mut keys = chars("smaller steps");
    keys.push(KeyCode::Enter);
    assert_eq!(
        answered(plan(), &keys),
        resolve(
            "i1",
            json!({"decision": "keep_planning", "feedback": "smaller steps"})
        )
    );
}

/// 板上的字与 Puffin 一字不差：板头、`N more waiting`、编号的回答；输入框的灰字与槽是请求的。
#[test]
fn the_board_says_what_puffin_says() {
    let start = Instant::now();
    let mut app = app(Lang::En);
    ask(
        &mut app,
        "i1",
        bash("cargo build", &["cargo build:*"]),
        start,
    );
    // 第二个请求是别的工具：同一工具的两个才多一个 Allow all（`allow_all_asks`）。
    let mut other = bash("cargo test", &[]);
    if let InteractionRequest::Permission {
        tool_use,
        tool,
        input,
        ..
    } = &mut other
    {
        *tool_use = ToolUseId::new("tu9");
        *tool = "Read".into();
        *input = json!({"file_path": "/Users/dev/project/README.md"});
    }
    ask(&mut app, "i2", other, start);
    let (rows, slot) = board(&app, start + ARMED);
    assert!(
        rows[0].trim_start().starts_with("Run this command?"),
        "{rows:#?}"
    );
    assert!(rows[0].ends_with("1 more waiting"), "{rows:#?}");
    let starts = |at: usize, text: &str| {
        assert!(
            rows[at].trim_start().starts_with(text),
            "{at}: {:?}",
            rows[at]
        );
    };
    starts(1, "1  Allow once");
    starts(2, "2  Always allow");
    assert!(rows[2].contains("Bash(cargo build:*)"), "{:?}", rows[2]);
    starts(3, "3  Full access");
    starts(4, "4  Deny");
    assert_eq!(slot, "1–4 answer  esc stop");
    assert_eq!(app.ask_placeholder(), Some("Tell it what to do instead"));
    // 打了字：⏎ 拒绝并带着字发出。
    press(&mut app, &chars("no"), start + ARMED);
    assert_eq!(board(&app, start + ARMED).1, "⏎ deny and send");
}

/// 旧界面多出的（工作线 F72）：选中 Always allow 时 ⇥ 把 Bash 的前缀放进输入框，⏎ 记住改过的前缀。
#[test]
fn the_bash_prefix_can_be_edited() {
    let start = Instant::now();
    let mut app = app(Lang::En);
    ask(
        &mut app,
        "i1",
        seeded(
            "git push origin main",
            &["git push origin main"],
            "git push *",
        ),
        start,
    );
    let at = start + ARMED;
    press(&mut app, &[KeyCode::Down], at);
    // 选中 Always allow：槽照旧是 Puffin 的（⇥ 改规则只列在 `?` 面板里，tui-kit.md §4.3）。
    assert_eq!(board(&app, at).1, "1–4 answer  esc stop");
    press(&mut app, &[KeyCode::Tab], at);
    assert_eq!(board(&app, at).1, "⏎ always allow  esc stop");
    // 改前缀：删到只剩第一个字再打（删光了就不再是前缀，⏎ 是 Deny，`settle_ask`）。
    let seeded = app.composer.area.text().to_owned();
    assert!(seeded.starts_with('g'), "{seeded:?}");
    for _ in 1..seeded.chars().count() {
        press(&mut app, &[KeyCode::Backspace], at);
    }
    press(&mut app, &chars("it push *"), at);
    assert_eq!(
        press(&mut app, &[KeyCode::Enter], at),
        resolve(
            "i1",
            json!({"decision": "allow", "remember": [{"tool": "Bash", "content": "git push *"}]})
        )
    );
}

/// 中文界面：板头与回答照翻译。
#[test]
fn the_chinese_board_is_translated() {
    let start = Instant::now();
    let mut app = app(Lang::Zh);
    ask(&mut app, "i1", bash("rm -rf build", &[]), start);
    let (rows, slot) = board(&app, start + ARMED);
    assert!(!rows[0].starts_with("Run this command?"), "{rows:#?}");
    assert!(
        rows.iter().all(|row| !row.contains("Allow once")),
        "{rows:#?}"
    );
    assert!(!slot.contains("answer"), "{slot}");
}

/// 照 Puffin（`AskUi.pinned`）：请求打开时视图钉在它的行，滚动一次放开；新的请求成为在回答的那个时重新钉住。`below` 时
/// 槽是 `PgDn read on  esc stop`。
#[test]
fn an_open_ask_pins_the_view_until_a_scroll() {
    let start = Instant::now();
    let mut app = app(Lang::En);
    assert!(!app.ask_pinned());
    ask(&mut app, "i1", bash("ls", &[]), start);
    assert!(app.ask_pinned());
    app.unpin_ask();
    assert!(!app.ask_pinned());
    let (laid, _, _) = app.ask_laid(100, start + ARMED, true).unwrap();
    let slot: String = laid.slot.iter().map(|span| &*span.text).collect();
    assert_eq!(
        slot.split_whitespace().collect::<Vec<_>>(),
        ["PgDn", "read", "on", "esc", "stop"]
    );
}
