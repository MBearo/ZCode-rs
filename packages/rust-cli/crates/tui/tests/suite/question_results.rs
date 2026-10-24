//! AskUserQuestion 的提问框（docs/specs/rust-cli/tools-more.md §1.1，工作线 F72）：带预览的题写备注、空闲到期按已选的
//! 答案自动提交，工具结果回到模型。
use std::sync::Arc;

use crossterm::event::KeyCode;
use rust_cli_kernel_api::testing::{text_response, tool_use_response};
use rust_cli_tui::ui::afk::QuestionTimeout;
use serde_json::{Value, json};

use super::support::{GatedModel, Setup, request_text, with_ui};

fn layout() -> Value {
    json!({
        "question": "Which layout should the page use?", "header": "Layout", "multiSelect": false,
        "options": [
            {"label": "Sidebar", "description": "Navigation on the left", "preview": "```\nnav | body\n```"},
            {"label": "Top bar", "description": "Navigation on top", "preview": "## Top bar\n\n- one\n- two"},
        ],
    })
}

fn color() -> Value {
    json!({
        "question": "Which color?", "header": "Color", "multiSelect": false,
        "options": [{"label": "Red", "description": "warm"}, {"label": "Blue", "description": "cool"}],
    })
}

fn asking(questions: Vec<Value>) -> Arc<GatedModel> {
    GatedModel::scripted(vec![
        tool_use_response(
            "toolu_1",
            "AskUserQuestion",
            &json!({ "questions": questions }),
        ),
        text_response(&["noted"]),
    ])
}

fn roomy() -> Setup {
    Setup {
        width: 120,
        height: 40,
        ..Setup::default()
    }
}

/// 带预览的题：左边选项、右边焦点所在选项的预览；移到第二个选项写备注、enter，结果带预览与备注。
#[tokio::test(flavor = "multi_thread")]
async fn notes_on_a_previewed_option_reach_the_model() {
    let model = asking(vec![layout()]);
    with_ui(model.clone(), roomy(), |mut ui| async move {
        ui.submit("pick a layout");
        let shown = ui.wait_for("Which layout should the page use?").await;
        assert!(shown.contains("❯ 1. Sidebar"), "{shown}");
        assert!(shown.contains("│ nav | body"), "{shown}");
        assert!(shown.contains("Notes: press n to add notes"), "{shown}");
        assert!(shown.contains("n to add notes · Esc to cancel"), "{shown}");
        ui.settle().await;
        ui.key(KeyCode::Down);
        ui.wait_for("❯ 2. Top bar").await;
        ui.key(KeyCode::Char('n'));
        ui.wait_for("Add notes on this design").await;
        ui.type_text("wider");
        ui.wait_for("Notes: wider").await;
        ui.key(KeyCode::Enter);
        ui.wait_for("noted").await;
    })
    .await;
    let second = request_text(&model.requests()[1]);
    assert!(
        second.contains(r#"The user answered: \"Which layout should the page use?\"=\"Top bar\" selected preview:\n## Top bar\n\n- one\n- two notes: wider. Read the answers carefully"#),
        "{second}"
    );
}

/// 配了空闲超时：弹出就显示倒计时（门槛不超过超时）；按键重新计时，到期按当时已选的答案提交。
#[tokio::test(flavor = "multi_thread")]
async fn an_idle_question_continues_with_what_was_selected() {
    let model = asking(vec![color(), layout()]);
    let setup = Setup {
        services: rust_cli_tui::Services {
            question_timeout: QuestionTimeout::new(None, Some(2_000), None),
            ..rust_cli_tui::Services::default()
        },
        ..roomy()
    };
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.submit("pick a color");
        let shown = ui.wait_for("auto-continue in 2s · any key to stay").await;
        assert!(shown.contains("Which color?"), "{shown}");
        ui.settle().await;
        ui.key(KeyCode::Enter);
        ui.wait_for("Which layout should the page use?").await;
        ui.wait_for("noted").await;
    })
    .await;
    let second = request_text(&model.requests()[1]);
    assert!(
        second.contains(r#"No response after 2s — the user may be away from keyboard. Proceed using your best judgment based on the context so far; you can re-ask this question later if it's still relevant.\n\nBefore going idle the user had selected: \"Which color?\"=\"Red\"."#),
        "{second}"
    );
}
