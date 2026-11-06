// 改写自 apps/zcode-host/crates/tui/tests/suite/host_flow.rs（feat/rs 79b6691c55）：经 TUI 的事件循环走
// 建会话 → 流式回复 → 中断（docs/specs/rust-cli/tui.md §4）。
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers};
use rust_cli_kernel_api::message::ContentBlock;
use rust_cli_kernel_api::model::{BlockDelta, ModelEvent};
use rust_cli_kernel_api::testing::{FixedClock, text_response};

use super::support::{GatedModel, Item, Setup, row_of, send, with_ui};

#[tokio::test(flavor = "multi_thread")]
async fn a_first_message_creates_the_session_and_streams_the_reply() {
    let model = GatedModel::scripted(vec![text_response(&["hel", "lo\n", "world"])]);
    let session = with_ui(model.clone(), Setup::default(), |mut ui| async move {
        let start = ui.wait_for("Ask before changes").await;
        // 启动头是 Colophon：版本与目录（模型在 footer 里），输入框已经说了要做什么（tui.md §7.3）。
        assert!(start.contains("ZCode  0.1.0"), "{start}");
        assert!(start.contains("~/project"), "{start}");
        assert!(!start.contains("Type a message"), "{start}");
        // Puffin 的欢迎块没有 `Try "…"` 的示例行。
        assert!(!start.contains("Try \""), "{start}");
        ui.submit("hi there");
        let screen = ui.wait_for("world").await;
        assert!(screen.contains(" › hi there"), "{screen}");
        assert!(screen.contains("hello"), "{screen}");
        // 回合结束：footer 不再提示中断。
        ui.wait("the turn to end", |s| !s.contains("esc stop"))
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
        assert!(running.contains("esc stop"), "{running}");
        // 没有常驻的状态行（以前写 token 数）：工作光标在 transcript 的末尾（tui-kit.md §4.0「尾巴」）。
        assert!(!running.contains("tokens"), "no status line: {running}");
        ui.key(KeyCode::Esc);
        // 回合停下之后它的最后一行下面是 Puffin 的 `Stopped.`（以前是 `Interrupted · What should ZCode do instead?`）。
        let screen = ui.wait_for("Stopped.").await;
        assert!(!screen.contains("What should ZCode do instead"), "{screen}");
        assert!(!screen.contains("esc stop"), "{screen}");
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
        ui.wait_for("Press ^C again to exit").await;
        ui.press(KeyCode::Char('c'), KeyModifiers::CONTROL);
        // 退出后事件循环不再画；脚本结束时它已经返回。
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    })
    .await;
    assert!(session.is_some());
}

/// 一个回合：思考 `weighing the options`，回答 `decided`。
fn thinking_turn() -> Vec<Item> {
    vec![
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
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn thinking_folds_into_a_summary_and_ctrl_o_shows_it() {
    let model = GatedModel::scripted(vec![thinking_turn()]);
    // 缺陷修复（0.1.86 发版验证，全量测试的负载下两次重试都失败）：思考的用时是界面时钟上思考行建立到消息收拢之差，
    // 过了 1 ms 收起的一句就是 `Thought for 1s`。真实时钟下这个测试只在两步落在同一毫秒时才看到光秃秃的 `Thought`，
    // 所以界面用固定时钟，用时恒为 0。
    let setup = Setup {
        ui_clock: Some(Arc::new(FixedClock(0))),
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("think");
        let screen = ui.wait_for("decided").await;
        // 结束的思考收成一段工作的一句（Puffin 的收拢；以前是 `Thought for Ns`）。
        assert!(screen.lines().any(|l| l.trim() == "Thought"), "{screen}");
        assert!(!screen.contains("weighing the options"), "{screen}");
        // ctrl+o 在原地换成详细的样子（Puffin `details_open`）：思考显示全文；再按一次换回来。以前打开占满屏幕的详细视图。
        ui.press(KeyCode::Char('o'), KeyModifiers::CONTROL);
        let detail = ui.wait_for("weighing the options").await;
        assert!(!detail.contains("Transcript  ›  Details"), "{detail}");
        assert!(!detail.contains("Showing detailed transcript"), "{detail}");
        assert!(detail.contains("decided"), "{detail}");
        ui.press(KeyCode::Char('o'), KeyModifiers::CONTROL);
        ui.wait("the details to close", |s| {
            !s.contains("weighing the options") && s.contains("decided")
        })
        .await;
    })
    .await;
}

/// 缺陷修复：点开的块（单独展开）以前在 ^O 打开又关上之后还开着，看起来是 ^O 收不起来。^O 关上时全部收起
/// （docs/puffin-tui.md §4.2「工作的收拢」的「展开」）。
#[tokio::test(flavor = "multi_thread")]
async fn ctrl_o_folds_a_block_opened_by_a_click() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let model = GatedModel::scripted(vec![thinking_turn()]);
    with_ui(model, Setup::default(), |mut ui| async move {
        ui.submit("think");
        let screen = ui
            .wait("the turn to end", |s| {
                s.contains("decided") && !s.contains("esc stop")
            })
            .await;
        // 点收拢行：只展开这一块。
        let row = row_of(&screen, "Thought").unwrap();
        let line = screen.lines().nth(usize::from(row)).unwrap();
        let col = u16::try_from(line.find("Thought").unwrap()).unwrap();
        ui.mouse(MouseEventKind::Down(MouseButton::Left), col, row);
        ui.mouse(MouseEventKind::Up(MouseButton::Left), col, row);
        ui.wait_for("weighing the options").await;
        ui.press(KeyCode::Char('o'), KeyModifiers::CONTROL);
        ui.press(KeyCode::Char('o'), KeyModifiers::CONTROL);
        ui.wait("every block to fold", |s| {
            !s.contains("weighing the options") && s.contains("decided")
        })
        .await;
    })
    .await;
}
