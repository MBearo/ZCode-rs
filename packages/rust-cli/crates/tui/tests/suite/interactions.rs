// 改写自 apps/zcode-host/crates/tui/tests/suite/interactions.rs（feat/rs 79b6691c55）：审批的回答由真实的
// 执行管线验证（docs/specs/rust-cli/tui.md §5.2）。模型发起 Bash 调用，TUI 按键作答，工具结果回到模型。
use crossterm::event::KeyCode;
use rust_cli_kernel_api::testing::{text_response, tool_use_response};
use serde_json::json;

use super::support::{GatedModel, Setup, request_text, with_ui};

fn bash_then_text() -> std::sync::Arc<GatedModel> {
    GatedModel::scripted(vec![
        tool_use_response(
            "toolu_1",
            "Bash",
            &json!({"command": "touch made-by-tool.txt", "description": "create a file"}),
        ),
        text_response(&["file step done"]),
    ])
}

#[tokio::test(flavor = "multi_thread")]
async fn an_approved_command_reaches_the_tool() {
    let model = bash_then_text();
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ui.submit("make a file");
        let ask = ui.wait_for("Bash command").await;
        assert!(ask.contains("touch made-by-tool.txt"), "{ask}");
        assert!(ask.contains("create a file"), "{ask}");
        assert!(ask.contains("Do you want to proceed?"), "{ask}");
        ui.settle().await;
        ui.key(KeyCode::Enter);
        ui.wait_for("file step done").await;
    })
    .await;
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    // 允许后工具真的执行了（测试环境没有执行环境，结果是执行环境的错误，不是拒绝）。
    let second = request_text(&requests[1]);
    assert!(second.contains("tool_result"), "{second}");
    assert!(!second.contains("doesn't want to proceed"), "{second}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_denied_command_tells_the_model_why() {
    let model = bash_then_text();
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ui.submit("make a file");
        ui.wait_for("Bash command").await;
        ui.settle().await;
        ui.key(KeyCode::Tab);
        ui.type_text("use make instead");
        ui.key(KeyCode::Enter);
        let screen = ui.wait_for("file step done").await;
        // Bash 并进工作段（复制来的 rust-tui.md §8.2.1）：一行摘要。
        assert!(screen.contains("Ran 1 shell command"), "{screen}");
    })
    .await;
    let second = request_text(&model.requests()[1]);
    assert!(second.contains("the user said:"), "{second}");
    assert!(second.contains("use make instead"), "{second}");
}

#[tokio::test(flavor = "multi_thread")]
async fn esc_on_a_permission_rejects_without_feedback() {
    let model = bash_then_text();
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ui.submit("make a file");
        ui.wait_for("Bash command").await;
        ui.settle().await;
        ui.key(KeyCode::Esc);
        ui.wait_for("file step done").await;
    })
    .await;
    let second = request_text(&model.requests()[1]);
    assert!(second.contains("doesn't want to proceed"), "{second}");
    assert!(!second.contains("the user said:"), "{second}");
}
