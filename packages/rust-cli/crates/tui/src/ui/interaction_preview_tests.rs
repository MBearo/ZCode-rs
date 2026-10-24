//! 带预览的题与空闲到期的回答（docs/specs/rust-cli/tools-more.md §1.1，工作线 F72）。
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::ids::{InteractionId, ToolUseId};
use rust_cli_kernel_api::orchestration::InteractionRequest;
use serde_json::{Value, json};

use crate::ui::i18n::{EN, Lang};
use crate::ui::interaction::{Interaction, View};
use crate::ui::keyboard::Typing;
use crate::ui::palette::{ColorLevel, Palette};
use crate::ui::text::plain;

const LAYOUT: &str = "Which layout should the page use?";
const TOP: &str = "## Top bar\n\n- one\n- two";

fn layout() -> Value {
    json!({
        "question": LAYOUT, "header": "Layout", "multiSelect": false,
        "options": [
            {"label": "Sidebar", "description": "Navigation on the left", "preview": "```\nnav | body\n```"},
            {"label": "Top bar", "description": "Navigation on top", "preview": TOP},
            {"label": "None", "description": "No navigation"},
        ],
    })
}

fn color() -> Value {
    json!({
        "question": "Which color?", "header": "Color", "multiSelect": false,
        "options": [{"label": "Red", "description": "warm"}, {"label": "Blue", "description": "cool"}],
    })
}

fn asked(questions: &[Value]) -> InteractionRequest {
    InteractionRequest::Question {
        tool_use: ToolUseId::new("tu1"),
        questions: Value::Array(questions.to_vec()),
    }
}

struct Driven {
    ui: Interaction,
    at: Instant,
}

impl Driven {
    fn new(request: &InteractionRequest) -> Self {
        let start = Instant::now();
        Self {
            ui: Interaction::new(InteractionId::new("i1"), request, start),
            at: start + Duration::from_millis(200),
        }
    }

    /// 依次按键，返回第一个回答。
    fn keys(&mut self, request: &InteractionRequest, keys: &[KeyCode]) -> Option<Value> {
        keys.iter().find_map(|code| {
            self.ui.on_key(
                request,
                KeyEvent::new(*code, KeyModifiers::NONE),
                self.at,
                Typing::default(),
            )
        })
    }

    fn shown(&self, request: &InteractionRequest) -> Vec<String> {
        let palette = Palette::new(ColorLevel::Ansi16, None);
        let view = View {
            palette: &palette,
            strings: &EN,
            lang: Lang::En,
            width: 120,
            height: 40,
            now_ms: 0,
        };
        self.ui.lines(request, &view).0.iter().map(plain).collect()
    }
}

fn chars(text: &str) -> Vec<KeyCode> {
    text.chars().map(KeyCode::Char).collect()
}

fn answered(answers: Value, annotations: Value) -> Option<Value> {
    Some(json!({"decision": "answered", "answers": answers, "annotations": annotations}))
}

/// 移到第二个选项、写备注、enter：选焦点所在的选项，带它的预览与备注。
#[test]
fn notes_after_moving_choose_the_focused_option() {
    let request = asked(&[layout()]);
    let mut keys = vec![KeyCode::Down, KeyCode::Char('n')];
    keys.extend(chars("wider"));
    keys.push(KeyCode::Enter);
    assert_eq!(
        Driven::new(&request).keys(&request, &keys),
        answered(
            json!({LAYOUT: "Top bar"}),
            json!({LAYOUT: {"preview": TOP, "notes": "wider"}})
        )
    );
}

/// 不移动直接写备注：答案是 `(notes only)`，只有备注。
#[test]
fn notes_without_moving_answer_with_notes_only() {
    let request = asked(&[layout()]);
    let mut keys = vec![KeyCode::Char('n')];
    keys.extend(chars(" wider "));
    keys.push(KeyCode::Enter);
    assert_eq!(
        Driven::new(&request).keys(&request, &keys),
        answered(
            json!({LAYOUT: "(notes only)"}),
            json!({LAYOUT: {"notes": "wider"}})
        )
    );
}

/// 数字只移动焦点；enter 选焦点所在的选项；↑ / ↓ 不回绕；没有预览的选项没有 preview。
#[test]
fn digits_and_arrows_only_move_the_focus() {
    let request = asked(&[layout()]);
    let mut driven = Driven::new(&request);
    assert_eq!(driven.keys(&request, &[KeyCode::Char('2')]), None);
    assert_eq!(driven.ui.cursor, 1);
    assert_eq!(
        driven.keys(&request, &[KeyCode::Enter]),
        answered(
            json!({LAYOUT: "Top bar"}),
            json!({LAYOUT: {"preview": TOP}})
        )
    );
    let mut driven = Driven::new(&request);
    assert_eq!(driven.keys(&request, &[KeyCode::Up, KeyCode::Up]), None);
    assert_eq!(driven.ui.cursor, 0);
    driven.keys(&request, &[KeyCode::Down, KeyCode::Down, KeyCode::Down]);
    assert_eq!(driven.ui.cursor, 2);
    assert_eq!(
        driven.keys(&request, &[KeyCode::Enter]),
        Some(json!({"decision": "answered", "answers": {LAYOUT: "None"}}))
    );
}

/// esc 先停止写备注，再按一次取消；退格删字。
#[test]
fn esc_stops_the_notes_before_cancelling() {
    let request = asked(&[layout()]);
    let mut driven = Driven::new(&request);
    let mut keys = vec![KeyCode::Char('n')];
    keys.extend(chars("wide"));
    keys.extend([KeyCode::Backspace, KeyCode::Esc]);
    assert_eq!(driven.keys(&request, &keys), None);
    assert!(!driven.ui.typing);
    assert_eq!(driven.ui.drafts[0].notes, "wid");
    assert!(
        driven
            .shown(&request)
            .iter()
            .any(|line| line.contains("Notes: wid")),
        "{:?}",
        driven.shown(&request)
    );
    assert_eq!(
        driven.keys(&request, &[KeyCode::Esc]),
        Some(json!({"decision": "declined"}))
    );
}

/// 两题：预览题 enter 到下一题，确认页提交两题的答案；空闲到期按当时已选的提交。
#[test]
fn several_questions_and_the_idle_answer() {
    let request = asked(&[layout(), color()]);
    let mut driven = Driven::new(&request);
    assert_eq!(
        driven.keys(&request, &[KeyCode::Down, KeyCode::Enter]),
        None
    );
    assert_eq!(driven.ui.question, 1);
    assert_eq!(
        driven.ui.idle_answer(&request, 60_000),
        Some(
            json!({"decision": "answered", "answers": {LAYOUT: "Top bar"},
            "annotations": {LAYOUT: {"preview": TOP}}, "afk_timeout_ms": 60_000})
        )
    );
    assert_eq!(driven.keys(&request, &[KeyCode::Enter]), None);
    assert_eq!(
        driven.keys(&request, &[KeyCode::Enter]),
        Some(json!({"decision": "answered",
            "answers": {LAYOUT: "Top bar", "Which color?": "Red"},
            "annotations": {LAYOUT: {"preview": TOP}}}))
    );
    // 回到预览题：焦点在选过的选项上。
    let mut driven = Driven::new(&request);
    driven.keys(&request, &[KeyCode::Down, KeyCode::Enter, KeyCode::Left]);
    assert_eq!((driven.ui.question, driven.ui.cursor), (0, 1));
    let idle = Driven::new(&request).ui.idle_answer(&request, 4_000);
    assert_eq!(
        idle,
        Some(json!({"decision": "answered", "answers": {}, "afk_timeout_ms": 4_000}))
    );
}

/// 排版：选项列、焦点所在选项的预览框、备注一行、按键提示与倒计时。
#[test]
fn the_preview_view_shows_the_focused_option() {
    let request = asked(&[layout()]);
    let mut driven = Driven::new(&request);
    let lines = driven.shown(&request);
    assert!(
        lines.iter().any(|line| line.trim() == "☐ Layout"),
        "{lines:?}"
    );
    let first = lines
        .iter()
        .find(|line| line.contains("❯ 1. Sidebar"))
        .unwrap_or_else(|| panic!("{lines:?}"));
    assert!(first.contains('┌'), "{lines:?}");
    assert!(
        lines.iter().any(|line| line.contains("│ nav | body")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Notes: press n to add notes")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|line| line
            .contains("Enter to select · ↑/↓ to navigate · n to add notes · Esc to cancel")),
        "{lines:?}"
    );
    driven.keys(&request, &[KeyCode::Down, KeyCode::Down]);
    driven.ui.countdown = Some(4);
    let lines = driven.shown(&request);
    assert!(
        lines
            .iter()
            .any(|line| line.contains("No preview available")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("❯ 3. None")),
        "{lines:?}"
    );
    let last = lines.last().cloned().unwrap_or_default();
    assert!(
        last.ends_with("auto-continue in 4s · any key to stay"),
        "{lines:?}"
    );
    // 两题时预览题选过后标签页打勾、选项绿色带 ✔。
    let request = asked(&[layout(), color()]);
    let mut driven = Driven::new(&request);
    driven.keys(&request, &[KeyCode::Enter, KeyCode::Left]);
    let lines = driven.shown(&request);
    assert!(
        lines.iter().any(|line| line.contains("☒ Layout")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("1. Sidebar ✔")),
        "{lines:?}"
    );
}
