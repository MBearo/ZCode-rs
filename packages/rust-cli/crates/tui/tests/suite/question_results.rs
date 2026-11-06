//! AskUserQuestion 的提问（docs/specs/rust-cli/tools-more.md §1.1，工作线 F72；样子与按键照 Puffin，tui.md §5.2）：带预览
//! 的题选了回答之后输入框里的字是备注、空闲到期按已选的答案自动提交，工具结果回到模型。
use std::sync::Arc;

use crossterm::event::KeyCode;
use rust_cli_kernel_api::testing::{text_response, tool_use_response};
use rust_cli_tui::QuestionTimeout;
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

/// 带预览的题：数字选中并翻页；←回到这一页、打字、⏎，字是这个回答的备注（Puffin 的单选页选了回答时，输入框的字是备注），
/// 结果带预览与备注。
#[tokio::test(flavor = "multi_thread")]
async fn notes_on_a_previewed_option_reach_the_model() {
    let model = asking(vec![layout(), color()]);
    with_ui(model.clone(), roomy(), |mut ui| async move {
        ui.submit("pick a layout");
        let shown = ui.wait_for("1  Sidebar").await;
        // 两页：板头是页名，槽有翻页的键。
        assert!(shown.contains("Layout  ·  Color"), "{shown}");
        assert!(shown.contains("1–2 pick  ←→ page"), "{shown}");
        ui.settle().await;
        ui.key(KeyCode::Char('2'));
        ui.wait_for("1  Red").await;
        ui.key(KeyCode::Left);
        ui.wait_for("1  Sidebar").await;
        ui.type_text("wider");
        ui.wait_for("› wider").await;
        ui.key(KeyCode::Enter);
        ui.wait_for("1  Red").await;
        ui.key(KeyCode::Char('1'));
        ui.wait_for("noted").await;
    })
    .await;
    let second = request_text(&model.requests()[1]);
    assert!(
        second.contains(r#"\"Which layout should the page use?\"=\"Top bar\" selected preview:\n## Top bar\n\n- one\n- two notes: wider, \"Which color?\"=\"Red\"."#),
        "{second}"
    );
}

/// 配了空闲超时：弹出就在槽里显示倒计时（门槛不超过超时）；按键重新计时，到期按当时已选的答案提交。
#[tokio::test(flavor = "multi_thread")]
async fn an_idle_question_continues_with_what_was_selected() {
    let model = asking(vec![color(), layout()]);
    let setup = Setup {
        ui: rust_cli_tui::UiConfig {
            question_timeout: QuestionTimeout::new(None, Some(2_000), None),
            ..rust_cli_tui::UiConfig::default()
        },
        ..roomy()
    };
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.submit("pick a color");
        let shown = ui.wait_for("auto-continue in 2s · any key to stay").await;
        assert!(shown.contains("1  Red"), "{shown}");
        ui.settle().await;
        ui.key(KeyCode::Enter);
        ui.wait_for("1  Sidebar").await;
        ui.wait_for("noted").await;
    })
    .await;
    let second = request_text(&model.requests()[1]);
    assert!(
        second.contains(r#"No response after 2s — the user may be away from keyboard. Proceed using your best judgment based on the context so far; you can re-ask this question later if it's still relevant.\n\nBefore going idle the user had selected: \"Which color?\"=\"Red\"."#),
        "{second}"
    );
}
