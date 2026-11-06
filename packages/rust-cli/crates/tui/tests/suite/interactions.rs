// 改写自 apps/zcode-host/crates/tui/tests/suite/interactions.rs（feat/rs 79b6691c55）：审批的回答由真实的
// 执行管线验证（docs/specs/rust-cli/tui.md §5.2）。模型发起 Bash 调用，TUI 按键作答，工具结果回到模型。
//! 请求照 Puffin（tui-kit.md §4.2「权限询问、提问、计划审批」）：板头 `Run this command?`，预览在正文里 Bash 行下面，
//! ⏎ 是选中的回答（打开时是 Allow once），打了字时 ⏎ 是 Deny 带着字，esc 停下回合。
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
        let ask = ui.wait_for("Run this command?").await;
        // 预览在正文里 Bash 行下面：命令就是行的摘要，所以只有说明（Puffin `permission_preview`）。没有建议规则时回答是
        // Allow once、Full access、Deny。
        let row = ask
            .lines()
            .position(|l| l.contains("● Bash  touch made-by-tool.txt"));
        let row = row.unwrap_or_else(|| panic!("{ask}"));
        assert_eq!(
            ask.lines().nth(row + 1).map(str::trim),
            Some("│ create a file"),
            "{ask}"
        );
        for answer in ["1  Allow once", "2  Full access", "3  Deny"] {
            assert!(ask.contains(answer), "{answer}: {ask}");
        }
        assert!(ask.contains("1–3 answer  esc stop"), "{ask}");
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

/// 打了字：Deny 选中，⏎ 带着字拒绝（Puffin 的 `deny and send`）。
#[tokio::test(flavor = "multi_thread")]
async fn a_denied_command_tells_the_model_why() {
    let model = bash_then_text();
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ui.submit("make a file");
        ui.wait_for("Run this command?").await;
        ui.settle().await;
        ui.type_text("use make instead");
        ui.wait_for("deny and send").await;
        ui.key(KeyCode::Enter);
        ui.wait_for("file step done").await;
    })
    .await;
    let second = request_text(&model.requests()[1]);
    assert!(second.contains("the user said:"), "{second}");
    assert!(second.contains("use make instead"), "{second}");
}

/// 选 Deny（数字）：不带意见拒绝。
#[tokio::test(flavor = "multi_thread")]
async fn the_deny_number_rejects_without_feedback() {
    let model = bash_then_text();
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ui.submit("make a file");
        ui.wait_for("Run this command?").await;
        ui.settle().await;
        ui.key(KeyCode::Char('3'));
        ui.wait_for("file step done").await;
    })
    .await;
    let second = request_text(&model.requests()[1]);
    assert!(second.contains("doesn't want to proceed"), "{second}");
    assert!(!second.contains("the user said:"), "{second}");
}

/// esc 停下回合（Puffin `Interrupt`）：请求随回合收回，模型不再被问。
#[tokio::test(flavor = "multi_thread")]
async fn esc_on_a_request_stops_the_turn() {
    let model = bash_then_text();
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ui.submit("make a file");
        ui.wait_for("Run this command?").await;
        ui.settle().await;
        ui.key(KeyCode::Esc);
        let screen = ui
            .wait("the request to leave", |s| !s.contains("Run this command?"))
            .await;
        assert!(!screen.contains("Answered in another window."), "{screen}");
    })
    .await;
    assert_eq!(model.requests().len(), 1);
}
