//! 跨会话（docs/specs/rust-cli/cross-session.md §2.6）：扣住的同伴消息的批准框（中英文）、批准后的 `@ 名字 ›` 行与
//! 回执、拒绝；深浅主题下名字的颜色。端口是测试替身：入站经通道推入，回执记下来；会话 actor 与界面都是真的。
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use crossterm::event::KeyCode;
use futures_util::stream::BoxStream;
use ratatui::style::Color;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::peers::{
    InboundPeerMessage, ModeClass, PeerAddress, PeerIdentity, PeerMessaging, PeerOutbound,
    PeerRecord, PeerSendError, PeerSignal, PeerStatus, ReceiptStatus, envelope,
};
use rust_cli_kernel_api::testing::text_response;
use rust_cli_tui::ui::palette::{ColorLevel, Palette};
use tokio::sync::mpsc;

use super::support::{GatedModel, Setup, with_ui};

struct FakePort {
    inbound: Mutex<Option<mpsc::UnboundedReceiver<PeerSignal>>>,
    sender: mpsc::UnboundedSender<PeerSignal>,
    receipts: Mutex<Vec<ReceiptStatus>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl FakePort {
    fn new() -> Arc<Self> {
        let (sender, receiver) = mpsc::unbounded_channel();
        Arc::new(Self {
            inbound: Mutex::new(Some(receiver)),
            sender,
            receipts: Mutex::default(),
        })
    }

    /// 发送方是 bypass、本会话是 default（prompting）：扣住。
    fn push_held(&self) {
        let from = PeerAddress::from_socket("/Users/dev/.zcode/rust-cli/peers/9.sock");
        let text = envelope(
            &from,
            Some("work-b-7f"),
            ModeClass::Bypass,
            "hello from B\nsecond line",
        );
        self.sender
            .send(PeerSignal::Message(InboundPeerMessage {
                seq: 1,
                msg_id: "m1".to_owned(),
                from,
                envelope: text,
                received_at_ms: 0,
            }))
            .unwrap();
    }
}

#[async_trait]
impl PeerMessaging for FakePort {
    async fn identity(&self) -> Option<PeerIdentity> {
        None
    }
    async fn peers(&self) -> Vec<PeerRecord> {
        Vec::new()
    }
    async fn send(&self, _message: PeerOutbound) -> Result<(), PeerSendError> {
        Ok(())
    }
    async fn receipt(&self, _to: PeerAddress, _msg_id: String, status: ReceiptStatus) {
        lock(&self.receipts).push(status);
    }
    async fn bind(&self, _session: &SessionId) {}
    async fn unbind(&self, _session: &SessionId) {}
    fn signals(&self, _session: &SessionId) -> BoxStream<'static, PeerSignal> {
        match lock(&self.inbound).take() {
            Some(receiver) => Box::pin(futures_util::stream::unfold(
                receiver,
                |mut receiver| async move { receiver.recv().await.map(|signal| (signal, receiver)) },
            )),
            None => Box::pin(futures_util::stream::pending()),
        }
    }
    async fn ack(&self, _session: &SessionId, _seq: u64) {}
    async fn set_status(&self, _session: &SessionId, _status: PeerStatus) {}
}

fn setup(port: &Arc<FakePort>, lang: &'static str, background: (u8, u8, u8)) -> Setup {
    Setup {
        lang,
        height: 32,
        palette: Palette::new(ColorLevel::TrueColor, Some(background)),
        peers: rust_cli_runtime::PeerDeps {
            port: port.clone(),
            hold_timeout_ms: 300_000,
            receipts_wake: true,
        },
        ..Setup::default()
    }
}

/// 画面里 `needle` 第一次出现处那一格的前景色。
fn color_of(ui: &super::support::Ui, needle: &str) -> Option<Color> {
    let buffer = ui.buffer.lock().unwrap().clone()?;
    let area = buffer.area;
    (area.top()..area.bottom()).find_map(|y| {
        let row: String = (area.left()..area.right())
            .map(|x| buffer[(x, y)].symbol())
            .collect();
        let at = row.find(needle)?;
        let column = row[..at].chars().count() as u16;
        Some(buffer[(area.left() + column, y)].fg)
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_held_message_asks_for_approval_and_is_delivered() {
    let port = FakePort::new();
    let model = GatedModel::scripted(vec![text_response(&["first"]), text_response(&["got it"])]);
    let found = Arc::new(Mutex::new(None));
    let seen = found.clone();
    let probe = port.clone();
    with_ui(
        model.clone(),
        setup(&port, "en_US.UTF-8", (0, 0, 0)),
        |mut ui| async move {
            ui.submit("start");
            ui.wait_for("first").await;
            probe.push_held();
            let screen = ui.wait_for("Held message from another session").await;
            assert!(
                screen.contains("Another ZCode session sent a message: from"),
                "{screen}"
            );
            assert!(
                screen.contains(
                    "uds:/Users/dev/.zcode/rust-cli/peers/9.sock (peer claims name: work-b-7f)"
                ),
                "{screen}"
            );
            assert!(
                screen.contains(
                    "The sending session's permission mode class doesn't match this session's"
                ),
                "{screen}"
            );
            assert!(
                screen.contains("Message body (this is what will be delivered):"),
                "{screen}"
            );
            assert!(screen.contains("hello from B"), "{screen}");
            assert!(
                screen.contains("1. Deny — drop it and tell the sender it was declined"),
                "{screen}"
            );
            assert!(
                screen.contains("2. Deliver this message to ZCode"),
                "{screen}"
            );
            ui.key(KeyCode::Down);
            ui.key(KeyCode::Enter);
            let screen = ui.wait_for("got it").await;
            assert!(screen.contains("@ work-b-7f › hello from B …"), "{screen}");
            assert!(
                !screen.contains("Held message from another session"),
                "{screen}"
            );
            *seen.lock().unwrap() = color_of(&ui, "@ work-b-7f");
        },
    )
    .await;
    assert_eq!(
        *lock(&port.receipts),
        [ReceiptStatus::Held, ReceiptStatus::Delivered]
    );
    // 投递的消息开了第二个回合：模型看到空闲的写法。
    let requests = model.requests();
    let last = super::support::request_text(requests.last().unwrap());
    // `request_text` 是请求体的 JSON：换行写作 `\n`。
    assert!(
        last.contains(r"Another ZCode session sent a message:\n<cross-session-message"),
        "{last}"
    );
    let dark = found.lock().unwrap().unwrap();

    // 浅色背景：名字的强调色换成浅色主题的那一种。
    let port = FakePort::new();
    let model = GatedModel::scripted(vec![text_response(&["first"]), text_response(&["got it"])]);
    let found = Arc::new(Mutex::new(None));
    let seen = found.clone();
    let probe = port.clone();
    with_ui(
        model,
        setup(&port, "en_US.UTF-8", (255, 255, 255)),
        |mut ui| async move {
            ui.submit("start");
            ui.wait_for("first").await;
            probe.push_held();
            ui.wait_for("Held message from another session").await;
            ui.key(KeyCode::Char('2'));
            ui.key(KeyCode::Enter);
            ui.wait_for("got it").await;
            *seen.lock().unwrap() = color_of(&ui, "@ work-b-7f");
        },
    )
    .await;
    let light = found.lock().unwrap().unwrap();
    assert_ne!(dark, light, "the name uses the theme accent");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_chinese_dialog_denies_with_escape() {
    let port = FakePort::new();
    let model = GatedModel::scripted(vec![text_response(&["第一句"])]);
    let probe = port.clone();
    with_ui(
        model,
        setup(&port, "zh_CN.UTF-8", (0, 0, 0)),
        |mut ui| async move {
            ui.submit("开始");
            ui.wait_for("第一句").await;
            probe.push_held();
            let screen = ui.wait_for("来自其他会话的消息已扣住").await;
            assert!(screen.contains("另一个 ZCode 会话发来消息"), "{screen}");
            assert!(
                screen.contains("发送方会话的权限模式类别与本会话不同"),
                "{screen}"
            );
            assert!(
                screen.contains("1. 拒绝——丢弃，并告诉发送方被拒绝"),
                "{screen}"
            );
            assert!(screen.contains("2. 把这条消息交给 ZCode"), "{screen}");
            ui.key(KeyCode::Esc);
            ui.wait("the dialog to close", |s| {
                !s.contains("来自其他会话的消息已扣住")
            })
            .await;
        },
    )
    .await;
    assert_eq!(
        *lock(&port.receipts),
        [ReceiptStatus::Held, ReceiptStatus::Denied]
    );
}

/// 批准框的焦点行（tui.md §7.7，工作线 F76，用户决定 2026-10-23）：与提问视图相同，不铺底色条，`❯` 与名字是强调色；
/// 深浅两种背景。旧代码整行铺底色条。
#[tokio::test(flavor = "multi_thread")]
async fn the_focused_choice_has_no_bar_in_both_themes() {
    for (background, accent) in [
        ((0, 0, 0), Color::Rgb(99, 168, 248)),
        ((255, 255, 255), Color::Rgb(28, 100, 200)),
    ] {
        let port = FakePort::new();
        let model = GatedModel::scripted(vec![text_response(&["first"])]);
        let probe = port.clone();
        with_ui(
            model,
            setup(&port, "en_US.UTF-8", background),
            |mut ui| async move {
                ui.submit("start");
                ui.wait_for("first").await;
                probe.push_held();
                ui.wait("the pointer on deny", |s| s.contains("❯ 1. Deny"))
                    .await;
                let buffer = ui.buffer.lock().unwrap().clone().unwrap();
                let area = buffer.area;
                let row = |needle: &str| {
                    (area.top()..area.bottom()).find(|&y| {
                        let text: String = (area.left()..area.right())
                            .map(|x| buffer[(x, y)].symbol())
                            .collect();
                        text.contains(needle)
                    })
                };
                let focused = row("❯ 1. Deny").unwrap();
                let other = row("2. Deliver").unwrap();
                for y in [focused, other] {
                    for x in area.left()..area.right() {
                        assert_eq!(buffer[(x, y)].bg, Color::Reset, "column {x} of row {y}");
                    }
                }
                assert_eq!(color_of(&ui, "❯ 1. Deny"), Some(accent));
                assert_eq!(color_of(&ui, "Deny —"), Some(accent));
                assert_eq!(color_of(&ui, "Deliver this"), Some(Color::Reset));
                ui.key(KeyCode::Esc);
            },
        )
        .await;
    }
}
