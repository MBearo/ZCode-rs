//! 快捷键（docs/specs/rust-cli/tui-input.md §5）：ctrl+enter 立即发送（中断后开新回合）、
//! ctrl+l 清空输入、ctrl+s 暂存与发送后放回、meta+p 打开模型选择、`/help` 的快捷键一节。
use crossterm::event::{KeyCode, KeyModifiers};
use rust_cli_kernel_api::testing::text_response;

use super::support::{GatedModel, Setup, finish, request_text, send, with_ui};

#[tokio::test(flavor = "multi_thread")]
async fn ctrl_enter_interrupts_and_sends_now() {
    let (model, senders) = GatedModel::new(2);
    let mut senders = senders.into_iter();
    let (first, second) = (senders.next().unwrap(), senders.next().unwrap());
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ui.submit("write a long essay");
        // 只发开头三个事件：回合停在流式回复中间。
        // 带换行：流式中没写完的最后一行不显示（`cells::messages::visible`）。
        let mut head = text_response(&["Once upon a time\n"]);
        let _tail = head.split_off(3);
        send(&first, head);
        // 修复线 F35：等流出的文字，不等 `esc to interrupt`（TurnStarted 时就画出，比输入提交与第一次请求早；在这段
        // 空档里中断时第一个回合没有请求，第二个回合拿到第一个通道，一直等不到回复）。
        ui.wait_for("Once upon a time").await;
        ui.type_text("stop, just say hi");
        ui.press(KeyCode::Enter, KeyModifiers::CONTROL);
        finish(second, text_response(&["hi"]));
        ui.wait("the new turn", |s| {
            s.contains("hi") && s.contains("❯ stop, just say hi") && !s.contains("esc to interrupt")
        })
        .await;
        drop(first);
    })
    .await;
    let requests = model.requests();
    assert_eq!(requests.len(), 2, "the first turn was interrupted");
    assert!(request_text(&requests[1]).contains("stop, just say hi"));
}

#[tokio::test(flavor = "multi_thread")]
async fn ctrl_l_clears_the_input_into_history() {
    let model = GatedModel::scripted(vec![]);
    with_ui(model, Setup::default(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.type_text("draft text");
        ui.wait_for("draft text").await;
        ui.press(KeyCode::Char('l'), KeyModifiers::CONTROL);
        ui.wait("the input cleared", |s| !s.contains("draft text"))
            .await;
        ui.key(KeyCode::Up);
        ui.wait_for("draft text").await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ctrl_s_stashes_and_restores_after_submit() {
    let model = GatedModel::scripted(vec![text_response(&["answer"])]);
    with_ui(model, Setup::default(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.type_text("first draft");
        ui.press(KeyCode::Char('s'), KeyModifiers::CONTROL);
        let screen = ui.wait_for("Stashed (auto-restores after submit)").await;
        assert!(!screen.contains("first draft"), "{screen}");
        ui.submit("quick question");
        let screen = ui.wait_for("Draft restored").await;
        assert!(screen.contains("first draft"), "{screen}");
        assert!(!screen.contains("Stashed (auto-restores"), "{screen}");
        ui.wait("the reply", |s| {
            s.contains("answer") && !s.contains("esc to interrupt")
        })
        .await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn meta_p_opens_the_model_picker_and_help_lists_shortcuts() {
    let model = GatedModel::scripted(vec![]);
    with_ui(model, Setup::default(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.press(KeyCode::Char('p'), KeyModifiers::ALT);
        ui.wait_for("Select model").await;
        ui.key(KeyCode::Esc);
        ui.wait("the picker closed", |s| !s.contains("Select model"))
            .await;
        ui.submit("/help");
        ui.wait_for("Slash commands").await;
        for _ in 0..80 {
            ui.key(KeyCode::Down);
        }
        let screen = ui.wait_for("Shortcuts").await;
        assert!(screen.contains("ctrl+x enter"), "{screen}");
    })
    .await;
}
