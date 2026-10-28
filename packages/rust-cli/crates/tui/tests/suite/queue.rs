//! 运行中提交（docs/specs/rust-cli/tui.md §5.1）：enter 插话，在工具边界并入当前回合；
//! ctrl+x enter 相同（只多一个 `wait`，工具边界照样并入，修复线 F13）；排队的插话下面写立即发送的键
//! （§5.1.1，修复线 F104）；输入框为空时按这个键投递排着的插话（修复线 F105，替身在 `queue_send_now.rs`）。
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers};
use rust_cli_kernel_api::env::BackgroundReason;
use rust_cli_kernel_api::testing::{environments, text_response, tool_use_response};
use serde_json::json;

use super::queue_send_now::MovesEnv;
use super::support::{GatedModel, Setup, finish, request_text, row_of, send, with_ui};

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
        ui.wait_for("esc to interrupt").await;
        ui.submit("also check the date");
        // 排队的输入显示在输入框上方（来自 QueueChanged），输入框的占位文字随之变化。
        let queued = ui.wait_for("Queued messages will be sent").await;
        assert!(queued.contains("❯ also check the date"), "{queued}");
        finish(first, tail);
        finish(second, text_response(&["all read"]));
        let screen = ui.wait_for("all read").await;
        assert!(screen.contains("❯ also check the date"), "{screen}");
        ui.wait("the turn to end", |s| !s.contains("esc to interrupt"))
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
        ui.wait_for("esc to interrupt").await;
        ui.type_text("then summarize");
        ui.press(KeyCode::Char('x'), KeyModifiers::CONTROL);
        ui.key(KeyCode::Enter);
        let queued = ui.wait_for("Queued messages will be sent").await;
        assert!(queued.contains("❯ then summarize"), "{queued}");
        finish(first, tail);
        finish(second, text_response(&["notes read"]));
        ui.wait_for("notes read").await;
        ui.wait("the turn to end", |s| !s.contains("esc to interrupt"))
            .await;
    })
    .await;
    let requests = model.requests();
    assert_eq!(requests.len(), 2, "the queued input joined the same turn");
    assert!(request_text(&requests[1]).contains("then summarize"));
}

/// 建会话时停住的存储：`create` 等测试放行（R5-9）。
struct SlowCreate {
    inner: std::sync::Arc<rust_cli_kernel_api::testing::MemoryStore>,
    gate: std::sync::Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl rust_cli_kernel_api::store::SessionStore for SlowCreate {
    async fn create(
        &self,
        meta: rust_cli_kernel_api::store::SessionMeta,
    ) -> Result<(), rust_cli_kernel_api::store::StoreError> {
        self.gate.notified().await;
        self.inner.create(meta).await
    }

    async fn commit(
        &self,
        session: &rust_cli_kernel_api::ids::SessionId,
        batch: rust_cli_kernel_api::store::FactBatch,
    ) -> Result<rust_cli_kernel_api::store::CommitReceipt, rust_cli_kernel_api::store::StoreError>
    {
        self.inner.commit(session, batch).await
    }

    async fn load(
        &self,
        session: &rust_cli_kernel_api::ids::SessionId,
    ) -> Result<rust_cli_kernel_api::store::LoadedSession, rust_cli_kernel_api::store::StoreError>
    {
        self.inner.load(session).await
    }

    async fn list(
        &self,
        filter: rust_cli_kernel_api::store::SessionFilter,
    ) -> Result<
        Vec<rust_cli_kernel_api::store::SessionSummary>,
        rust_cli_kernel_api::store::StoreError,
    > {
        self.inner.list(filter).await
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
        session_store: Some(std::sync::Arc::new(SlowCreate {
            inner: std::sync::Arc::default(),
            gate: gate.clone(),
        })),
        ..Setup::default()
    };
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.submit("first message");
        ui.wait_for("Starting the session").await;
        ui.submit("second message");
        // 输入框空了（占位文字回来），这条显示在输入框上方。修复前：enter 被忽略，文字留在输入框里，也不排队。
        ui.wait("the input to be held", |s| {
            s.contains("❯ second message") && s.contains("Starting the session")
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

/// 修复线 F104（tui.md §5.1.1，用户要求 2026-10-27）：运行中有排队的插话时，队列项下面、输入框上面写一行
/// 立即发送的键；回合结束、队列清空之后这一行消失。旧代码没有这一行。
#[tokio::test(flavor = "multi_thread")]
async fn the_send_now_key_is_shown_under_the_queued_input() {
    let (model, senders) = GatedModel::new(2);
    let mut senders = senders.into_iter();
    let (first, second) = (senders.next().unwrap(), senders.next().unwrap());
    with_ui(model, Setup::default(), |mut ui| async move {
        ui.submit("read my notes");
        let (head, tail) = read_call();
        send(&first, head);
        ui.wait_for("esc to interrupt").await;
        ui.submit("also check the date");
        let screen = ui.wait_for("ctrl+x ctrl+s to send now").await;
        let item = row_of(&screen, "❯ also check the date").expect("the queued input");
        let hint = row_of(&screen, "ctrl+x ctrl+s to send now").expect("the hint");
        let input = row_of(&screen, "Queued messages will be sent").expect("the input box");
        assert_eq!(
            hint,
            item + 1,
            "the hint is right under the queue: {screen}"
        );
        assert!(hint < input, "the hint is above the input box: {screen}");
        finish(first, tail);
        finish(second, text_response(&["all read"]));
        // 插话并入回合、队列清空：这一行跟着没了（回合还在跑）。
        ui.wait("the hint to go", |s| !s.contains("to send now"))
            .await;
        ui.wait("the turn to end", |s| !s.contains("esc to interrupt"))
            .await;
    })
    .await;
}

/// 修复线 F104：终端分辨得出 ctrl+enter（进界面时键盘增强的查询为真）时提示写 ctrl+enter（tui-input.md §11.1.1）。
#[tokio::test(flavor = "multi_thread")]
async fn an_enhanced_terminal_shows_ctrl_enter_in_the_hint() {
    let (model, senders) = GatedModel::new(2);
    let mut senders = senders.into_iter();
    let (first, _second) = (senders.next().unwrap(), senders.next().unwrap());
    let setup = Setup {
        keyboard_enhanced: true,
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("read my notes");
        let (head, _tail) = read_call();
        send(&first, head);
        ui.wait_for("esc to interrupt").await;
        ui.submit("also check the date");
        let screen = ui.wait_for("ctrl+enter to send now").await;
        assert!(!screen.contains("ctrl+x ctrl+s"), "{screen}");
    })
    .await;
}

/// 修复线 F105（tui.md §5.1.1、tui-input.md §11.1，用户决定 2026-10-27）：输入框为空、队列里有插话时按立即发送的键，
/// 只把前台命令转到后台（`DeliverMessage`），不入队新文字、不中断回合；插话在工具边界并入。队列为空时按它什么都不做。
/// 旧代码空输入时什么都不发，执行环境收不到转后台的请求。
#[tokio::test(flavor = "multi_thread")]
async fn an_empty_input_delivers_the_queued_input_now() {
    let env = Arc::new(MovesEnv::default());
    let setup = Setup {
        environments: environments(env.clone()),
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
        ui.wait_for("esc to interrupt").await;
        // 队列为空：这一按什么都不做（之后的计数只有一次）。
        ui.press(KeyCode::Char('x'), KeyModifiers::CONTROL);
        ui.press(KeyCode::Char('s'), KeyModifiers::CONTROL);
        ui.submit("also check the date");
        ui.wait_for("ctrl+x ctrl+s to send now").await;
        ui.press(KeyCode::Char('x'), KeyModifiers::CONTROL);
        ui.press(KeyCode::Char('s'), KeyModifiers::CONTROL);
        probe.wait_moved(1).await;
        // 回合没有被中断：照常写完，插话在工具边界并入。
        finish(first, tail);
        finish(second, text_response(&["all read"]));
        ui.wait("the turn to end", |s| {
            s.contains("all read") && !s.contains("esc to interrupt")
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
