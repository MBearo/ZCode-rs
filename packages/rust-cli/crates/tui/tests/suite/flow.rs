// 改写自 apps/zcode-host/crates/tui/tests/suite/host_flow.rs（feat/rs 79b6691c55）：经 TUI 的事件循环走
// 建会话 → 流式回复 → 中断（docs/specs/rust-cli/tui.md §4）。
use crossterm::event::{KeyCode, KeyModifiers};
use rust_cli_kernel_api::message::ContentBlock;
use rust_cli_kernel_api::model::{BlockDelta, ModelEvent};
use rust_cli_kernel_api::testing::text_response;

use super::support::{GatedModel, Setup, send, with_ui};

#[tokio::test(flavor = "multi_thread")]
async fn a_first_message_creates_the_session_and_streams_the_reply() {
    let model = GatedModel::scripted(vec![text_response(&["hel", "lo\n", "world"])]);
    let session = with_ui(model.clone(), Setup::default(), |mut ui| async move {
        let start = ui.wait_for("Ask before changes").await;
        assert!(start.contains("ZCode v0.1.0"), "{start}");
        assert!(start.contains("scripted · ~/project"), "{start}");
        assert!(start.contains("Try \""), "{start}");
        ui.submit("hi there");
        let screen = ui.wait_for("world").await;
        assert!(screen.contains("❯ hi there"), "{screen}");
        assert!(screen.contains("hello"), "{screen}");
        // 回合结束：footer 不再提示中断。
        ui.wait("the turn to end", |s| !s.contains("esc to interrupt"))
            .await;
    })
    .await;
    assert!(session.is_some(), "the session id is returned on exit");
    let requests = model.requests();
    assert_eq!(requests.len(), 1);
    assert!(super::support::request_text(&requests[0]).contains("hi there"));
}

#[tokio::test(flavor = "multi_thread")]
async fn esc_interrupts_a_running_turn() {
    let (model, mut senders) = GatedModel::new(1);
    let sender = senders.remove(0);
    with_ui(model, Setup::default(), |mut ui| async move {
        ui.submit("slow please");
        // 第一段流出后停住：回合在运行。
        let mut events = text_response(&["partial line\n"]);
        events.truncate(3);
        send(&sender, events);
        let running = ui.wait_for("partial line").await;
        assert!(running.contains("esc to interrupt"), "{running}");
        assert!(running.contains("tokens"), "status line: {running}");
        ui.key(KeyCode::Esc);
        let screen = ui
            .wait_for("Interrupted · What should ZCode do instead?")
            .await;
        assert!(!screen.contains("esc to interrupt"), "{screen}");
        drop(sender);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ctrl_c_twice_exits_and_a_model_failure_shows_its_reason() {
    let failure = vec![Err(rust_cli_kernel_api::model::ModelError::Overloaded {
        message: "busy right now".into(),
    })];
    let model = GatedModel::scripted(vec![failure]);
    let session = with_ui(model, Setup::default(), |mut ui| async move {
        ui.submit("hi");
        ui.wait_for("busy right now").await;
        ui.press(KeyCode::Char('c'), KeyModifiers::CONTROL);
        ui.wait_for("Press Ctrl-C again to exit").await;
        ui.press(KeyCode::Char('c'), KeyModifiers::CONTROL);
        // 退出后事件循环不再画；脚本结束时它已经返回。
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    })
    .await;
    assert!(session.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn thinking_folds_into_a_summary_and_ctrl_o_shows_it() {
    let thinking = vec![
        Ok(ModelEvent::MessageStart {
            id: "msg".into(),
            model: "scripted".into(),
            usage: Default::default(),
        }),
        Ok(ModelEvent::BlockStart {
            index: 0,
            block: ContentBlock::Thinking {
                thinking: String::new(),
                signature: None,
            },
        }),
        Ok(ModelEvent::BlockDelta {
            index: 0,
            delta: BlockDelta::Thinking("weighing the options".into()),
        }),
        Ok(ModelEvent::BlockStop { index: 0 }),
        Ok(ModelEvent::BlockStart {
            index: 1,
            block: ContentBlock::Text {
                text: String::new(),
            },
        }),
        Ok(ModelEvent::BlockDelta {
            index: 1,
            delta: BlockDelta::Text("decided".into()),
        }),
        Ok(ModelEvent::BlockStop { index: 1 }),
        Ok(ModelEvent::MessageDelta {
            stop_reason: Some(rust_cli_kernel_api::message::StopReason::EndTurn),
            usage: Default::default(),
        }),
        Ok(ModelEvent::MessageStop),
    ];
    let model = GatedModel::scripted(vec![thinking]);
    with_ui(model, Setup::default(), |mut ui| async move {
        ui.submit("think");
        let screen = ui.wait_for("decided").await;
        assert!(screen.contains("Thought for"), "{screen}");
        assert!(!screen.contains("weighing the options"), "{screen}");
        ui.press(KeyCode::Char('o'), KeyModifiers::CONTROL);
        let detail = ui.wait_for("Showing detailed transcript").await;
        assert!(detail.contains("weighing the options"), "{detail}");
        ui.key(KeyCode::Esc);
        ui.wait("the detail view to close", |s| {
            !s.contains("detailed transcript")
        })
        .await;
    })
    .await;
}
