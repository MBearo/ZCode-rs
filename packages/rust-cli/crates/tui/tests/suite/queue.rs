//! 运行中提交（docs/specs/rust-cli/tui.md §5.1）：enter 插话，在工具边界并入当前回合；
//! ctrl+x enter 相同（只多一个 `wait`，工具边界照样并入，修复线 F13）；⇥ 排到这个回合结束之后、⌫ 取回（Puffin 的两条车道
//! 与「取回」，tui-kit.md §4.2「排队预览」）；输入框为空时按立即发送的键投递排着的插话（修复线 F105，替身在
//! `queue_send_now.rs`）。以前插话下面还有一行立即发送的键（F104），照 Puffin 删掉。
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers};
use rust_cli_kernel_api::env::BackgroundReason;
use rust_cli_kernel_api::testing::{text_response, tool_use_response};
use serde_json::json;

use super::queue_send_now::MovesEnv;
use super::support::{GatedModel, Setup, finish, request_text, row_of, send, with_ui};

/// 插进这一回合的消息下面的一句（Puffin 的尾巴）。
const STEERED: &str = "Queued. The agent reads it at its next step.";

/// 一次 Read 调用：先发开头两个事件（回合在运行），其余由测试在输入之后发出。
fn read_call() -> (Vec<super::support::Item>, Vec<super::support::Item>) {
    let mut head = tool_use_response(
        "toolu_1",
        "Read",
        &json!({"file_path": "/Users/dev/project/notes.txt"}),
    );
    let tail = head.split_off(2);
    (head, tail)
}

#[tokio::test(flavor = "multi_thread")]
async fn enter_while_running_steers_into_the_current_turn() {
    let (model, senders) = GatedModel::new(2);
    let mut senders = senders.into_iter();
    let (first, second) = (senders.next().unwrap(), senders.next().unwrap());
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ui.submit("read my notes");
        let (head, tail) = read_call();
        send(&first, head);
        ui.wait_for("esc stop").await;
        ui.submit("also check the date");
        // 插话画成尾巴里你的回合，下面是 `Queued. …`（来自 QueueChanged，Puffin 的尾巴）。
        let queued = ui.wait_for(STEERED).await;
        assert!(queued.contains("› also check the date"), "{queued}");
        finish(first, tail);
        finish(second, text_response(&["all read"]));
        let screen = ui.wait_for("all read").await;
        assert!(screen.contains("› also check the date"), "{screen}");
        ui.wait("the turn to end", |s| !s.contains("esc stop"))
            .await;
    })
    .await;
    let requests = model.requests();
    assert_eq!(requests.len(), 2, "the steer joined the same turn");
    assert!(request_text(&requests[1]).contains("also check the date"));
}

/// 修复线 F13：ctrl+x enter 原来按 `Queue` 排到回合结束后，多出一个回合；现在在工具
/// 边界照样并入它。
#[tokio::test(flavor = "multi_thread")]
async fn ctrl_x_enter_while_running_also_joins_at_the_tool_boundary() {
    let (model, senders) = GatedModel::new(2);
    let mut senders = senders.into_iter();
    let (first, second) = (senders.next().unwrap(), senders.next().unwrap());
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ui.submit("read my notes");
        let (head, tail) = read_call();
        send(&first, head);
        ui.wait_for("esc stop").await;
        ui.type_text("then summarize");
        ui.press(KeyCode::Char('x'), KeyModifiers::CONTROL);
        ui.key(KeyCode::Enter);
        let queued = ui.wait_for(STEERED).await;
        assert!(queued.contains("› then summarize"), "{queued}");
        finish(first, tail);
        finish(second, text_response(&["notes read"]));
        ui.wait_for("notes read").await;
        ui.wait("the turn to end", |s| !s.contains("esc stop"))
            .await;
    })
    .await;
    let requests = model.requests();
    assert_eq!(requests.len(), 2, "the queued input joined the same turn");
    assert!(request_text(&requests[1]).contains("then summarize"));
}

/// 建会话时停住的存储：`create` 等测试放行（R5-9）。
struct SlowCreate {
    gate: std::sync::Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl rust_cli_kernel_api::testing::StoreHooks for SlowCreate {
    async fn before(
        &self,
        call: rust_cli_kernel_api::testing::StoreCall<'_>,
    ) -> Result<(), rust_cli_kernel_api::store::StoreError> {
        if let rust_cli_kernel_api::testing::StoreCall::Create(_) = call {
            self.gate.notified().await;
        }
        Ok(())
    }
}

/// R5-9：新会话还在建时再按 enter，输入不被丢掉：先显示在输入框上方（与运行中排队的一样），会话建好后按顺序
/// 发出——第一条开回合，第二条排在它后面成为下一个回合。
#[tokio::test(flavor = "multi_thread")]
async fn input_while_the_session_is_starting_is_held_and_sent_in_order() {
    let model = GatedModel::scripted(vec![
        text_response(&["reply one"]),
        text_response(&["reply two"]),
    ]);
    let gate = std::sync::Arc::new(tokio::sync::Notify::new());
    let setup = Setup {
        session_store: Some(std::sync::Arc::new(
            rust_cli_kernel_api::testing::StoreProxy::new(
                std::sync::Arc::new(rust_cli_kernel_api::testing::MemoryStore::default()),
                SlowCreate { gate: gate.clone() },
            ),
        )),
        ..Setup::default()
    };
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.submit("first message");
        ui.submit("second message");
        // 输入框空了（占位文字回来），这条在尾巴里，下面说会话就绪就发出（Puffin 的尾巴）。修复前：enter 被忽略，文字留在
        // 输入框里，也不排队。以前输入框的占位写 `Starting the session…`。
        ui.wait("the input to be held", |s| {
            s.contains("› second message")
                && s.contains("Queued. It goes out as soon as the agent is ready.")
                && s.contains("Send a message")
        })
        .await;
        gate.notify_one();
        ui.wait_for("reply two").await;
    })
    .await;
    let requests = model.requests();
    assert_eq!(requests.len(), 2, "each input started its own turn");
    assert!(request_text(&requests[0]).contains("first message"));
    assert!(!request_text(&requests[0]).contains("second message"));
    assert!(request_text(&requests[1]).contains("second message"));
}

/// ⇥（Puffin 的第二条车道「after this turn」）：回合进行中把草稿排到这个回合结束之后，它在板上的栈里（右沿 `next turn`），
/// 槽是 `⌫ take back  esc stop`；⌫ 取回它（内核出队），文字回到草稿，槽是两条车道的键。旧界面的 ⇥ 在没有面板时什么都不做。
#[tokio::test(flavor = "multi_thread")]
async fn tab_stacks_the_draft_after_this_turn_and_backspace_takes_it_back() {
    let (model, senders) = GatedModel::new(3);
    let mut senders = senders.into_iter();
    let (first, second, third) = (
        senders.next().unwrap(),
        senders.next().unwrap(),
        senders.next().unwrap(),
    );
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ui.submit("read my notes");
        let (head, tail) = read_call();
        send(&first, head);
        ui.wait_for("esc stop").await;
        ui.type_text("then update the changelog");
        ui.wait_for("⏎ steer  ⇥ after this turn").await;
        ui.key(KeyCode::Tab);
        let screen = ui
            .wait("the stack", |s| {
                s.contains("› then update the changelog") && s.contains("⌫ take back  esc stop")
            })
            .await;
        let row = row_of(&screen, "› then update the changelog").expect("the stacked row");
        assert!(
            screen
                .lines()
                .nth(usize::from(row))
                .is_some_and(|l| l.trim_end().ends_with("next turn")),
            "{screen}"
        );
        assert!(!screen.contains(STEERED), "{screen}");
        ui.key(KeyCode::Backspace);
        ui.wait("the draft back", |s| {
            s.contains("› then update the changelog") && s.contains("⏎ steer  ⇥ after this turn")
        })
        .await;
        // 再 ⇥ 一次：这次留在栈上，回合结束后作为下一个回合发出。
        ui.key(KeyCode::Tab);
        ui.wait_for("⌫ take back  esc stop").await;
        finish(first, tail);
        finish(second, text_response(&["notes read"]));
        finish(third, text_response(&["changelog updated"]));
        ui.wait_for("changelog updated").await;
    })
    .await;
    let requests = model.requests();
    assert_eq!(requests.len(), 3, "the stacked draft is a turn of its own");
    assert!(!request_text(&requests[1]).contains("then update the changelog"));
    assert!(request_text(&requests[2]).contains("then update the changelog"));
}

/// 修复线 F105（tui.md §5.1.1、tui-input.md §11.1，用户决定 2026-10-27）：输入框为空、队列里有插话时按立即发送的键，
/// 只把前台命令转到后台（`DeliverMessage`），不入队新文字、不中断回合；插话在工具边界并入。队列为空时按它什么都不做。
/// 旧代码空输入时什么都不发，执行环境收不到转后台的请求。
#[tokio::test(flavor = "multi_thread")]
async fn an_empty_input_delivers_the_queued_input_now() {
    let env = Arc::new(MovesEnv::default());
    let setup = Setup {
        environments: env.clone().registry(),
        ..Setup::default()
    };
    let (model, senders) = GatedModel::new(2);
    let mut senders = senders.into_iter();
    let (first, second) = (senders.next().unwrap(), senders.next().unwrap());
    let probe = env.clone();
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.submit("read my notes");
        let (head, tail) = read_call();
        send(&first, head);
        ui.wait_for("esc stop").await;
        // 队列为空：这一按什么都不做（之后的计数只有一次）。
        ui.press(KeyCode::Char('x'), KeyModifiers::CONTROL);
        ui.press(KeyCode::Char('s'), KeyModifiers::CONTROL);
        ui.submit("also check the date");
        ui.wait_for(STEERED).await;
        ui.press(KeyCode::Char('x'), KeyModifiers::CONTROL);
        ui.press(KeyCode::Char('s'), KeyModifiers::CONTROL);
        probe.wait_moved(1).await;
        // 回合没有被中断：照常写完，插话在工具边界并入。
        finish(first, tail);
        finish(second, text_response(&["all read"]));
        ui.wait("the turn to end", |s| {
            s.contains("all read") && !s.contains("esc stop")
        })
        .await;
    })
    .await;
    assert_eq!(env.moved(), [BackgroundReason::DeliverMessage]);
    let requests = model.requests();
    assert_eq!(requests.len(), 2, "no extra turn");
    let second = request_text(&requests[1]);
    assert!(second.contains("also check the date"), "{second}");
    assert!(!second.contains("[Request interrupted"), "{second}");
}
