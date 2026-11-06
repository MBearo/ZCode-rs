//! 跨会话（docs/specs/rust-cli/cross-session.md §2.6）：扣住的同伴消息的批准框（中英文）、批准后的 `@ 名字 ›` 行与
//! 回执、拒绝；深浅主题下名字的颜色。端口是测试替身：入站经通道推入，回执记下来；会话 actor 与界面都是真的。
use rust_cli_base::sync::LockUnpoisoned;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use crossterm::event::KeyCode;
use futures_util::stream::BoxStream;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::peers::{
    InboundPeerMessage, ModeClass, PeerAddress, PeerIdentity, PeerMessaging, PeerOutbound,
    PeerRecord, PeerSendError, PeerSignal, PeerStatus, ReceiptStatus, envelope,
};
use rust_cli_kernel_api::testing::text_response;
use rust_cli_tui::{ColorLevel, Colors};
use rust_cli_tui_kit::render::Color;
use tokio::sync::mpsc;

use super::support::{GatedModel, Setup, with_ui};

struct FakePort {
    inbound: Mutex<Option<mpsc::UnboundedReceiver<PeerSignal>>>,
    sender: mpsc::UnboundedSender<PeerSignal>,
    receipts: Mutex<Vec<ReceiptStatus>>,
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
        self.receipts.lock_unpoisoned().push(status);
    }
    async fn bind(&self, _session: &SessionId) {}
    async fn unbind(&self, _session: &SessionId) {}
    fn signals(&self, _session: &SessionId) -> BoxStream<'static, PeerSignal> {
        match self.inbound.lock_unpoisoned().take() {
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
        colors: Colors::new(ColorLevel::TrueColor, Some(background)),
        peers: rust_cli_runtime::PeerDeps {
            port: port.clone(),
            hold_timeout_ms: 300_000,
            receipts_wake: true,
        },
        ..Setup::default()
    }
}

/// 画面里从以 `first` 开头的那一行起的 `n` 行（去掉行尾空格）。
fn block(screen: &str, first: &str, n: usize) -> Vec<String> {
    let lines: Vec<String> = screen.lines().map(|l| l.trim_end().to_owned()).collect();
    let at = lines
        .iter()
        .position(|l| l.starts_with(first))
        .unwrap_or(lines.len());
    lines.into_iter().skip(at).take(n).collect()
}

/// 画面里 `needle` 第一次出现处那一格的前景色。
fn color_of(ui: &super::support::Ui, needle: &str) -> Option<Color> {
    let buffer = ui.shot.lock().unwrap().clone()?;
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

/// 报告了前景的设置：rich 的主题（tui-kit.md §3.4；只报告背景时是 plain，深浅两种地的强调色相同）。
fn rich(mut setup: Setup, foreground: (u8, u8, u8)) -> Setup {
    setup.colors = setup.colors.with_foreground(Some(foreground));
    setup
}

#[tokio::test(flavor = "multi_thread")]
async fn a_held_message_asks_for_approval_and_is_delivered() {
    let port = FakePort::new();
    let model = GatedModel::scripted(vec![text_response(&["first"]), text_response(&["got it"])]);
    let probe = port.clone();
    with_ui(
        model.clone(),
        rich(setup(&port, "en_US.UTF-8", (0, 0, 0)), (255, 255, 255)),
        |mut ui| async move {
            ui.submit("start");
            ui.wait_for("first").await;
            probe.push_held();
            let screen = ui.wait_for("Deliver this message?").await;
            // 旧界面的画面（Puffin 没有这一面，tui-kit.md §4.3「同伴消息的批准」）：正文末尾是没有工具行的请求的
            // `● Peer  名字` 与轨上的发送方、原因与全文；板上是问句的板头与名字、说明两列的回答，投递在前。
            assert_eq!(
                block(&screen, " ● Peer  work-b-7f", 7),
                [
                    " ● Peer  work-b-7f",
                    "   │ Another ZCode session sent a message: from",
                    "   │ uds:/Users/dev/.zcode/rust-cli/peers/9.sock (peer claims name: work-b-7f)",
                    "   │ The sending session's permission mode class doesn't match this session's,",
                    "   │ so it wasn't delivered automatically.",
                    "   │ hello from B",
                    "   │ second line",
                ],
                "{screen}"
            );
            assert_eq!(
                block(&screen, "   Deliver this message?", 4),
                [
                    "   Deliver this message?",
                    "   1  Deliver   the agent reads it as a message",
                    "   2  Deny      drop it and tell the sender",
                    " ›  Send a message                                         1–2 answer  esc deny",
                ],
                "{screen}"
            );
            // 回答点亮之后才起作用（12 个 tick，tui.md §5.2）；焦点缺省在第一个回答（投递，同 Puffin 的请求）。
            ui.settle().await;
            ui.key(KeyCode::Enter);
            let screen = ui.wait_for("got it").await;
            // 投递之后是正文里的同伴消息（画法归 transcript，tui-kit.md §4.2），批准的板走了。
            assert!(screen.contains("Peer  work-b-7f  says"), "{screen}");
            assert!(!screen.contains("Deliver this message?"), "{screen}");
        },
    )
    .await;
    assert_eq!(
        *port.receipts.lock_unpoisoned(),
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
            let screen = ui.wait_for("投递这条消息？").await;
            assert!(screen.contains("另一个 ZCode 会话发来消息"), "{screen}");
            assert!(
                screen.contains("发送方会话的权限模式类别与本会话不同"),
                "{screen}"
            );
            assert!(
                screen.contains("1  投递   agent 把它当作一条消息读"),
                "{screen}"
            );
            assert!(screen.contains("2  拒绝   丢弃，并告诉发送方"), "{screen}");
            assert!(screen.contains("1–2 回答  esc 拒绝"), "{screen}");
            ui.key(KeyCode::Esc);
            ui.wait("the dialog to close", |s| !s.contains("投递这条消息？"))
                .await;
        },
    )
    .await;
    assert_eq!(
        *port.receipts.lock_unpoisoned(),
        [ReceiptStatus::Held, ReceiptStatus::Denied]
    );
}

/// 批准框的焦点行（tui.md §7.7）。样子照 Puffin 的「请求」（工作线 TK 的阶段 S，tui-kit.md §4.3「同伴消息的批准框」）：
/// 回答在 panel 底色的板上，焦点行的序号与名字是 brand，其余名字 fg2；深浅两种背景。
#[tokio::test(flavor = "multi_thread")]
async fn the_focused_choice_has_no_bar_in_both_themes() {
    // 报告了前景，主题是 rich（tui-kit.md §3.4）。
    for (background, foreground) in [((0, 0, 0), (255, 255, 255)), ((255, 255, 255), (0, 0, 0))] {
        let colors =
            Colors::new(ColorLevel::TrueColor, Some(background)).with_foreground(Some(foreground));
        let theme = colors.ink().theme;
        let port = FakePort::new();
        let model = GatedModel::scripted(vec![text_response(&["first"])]);
        let probe = port.clone();
        with_ui(
            model,
            rich(setup(&port, "en_US.UTF-8", background), foreground),
            |mut ui| async move {
                ui.submit("start");
                ui.wait_for("first").await;
                probe.push_held();
                ui.wait_for("1  Deliver").await;
                // 回答从 line 色淡入（12 个 tick，tui.md §5.2）：等它点亮。焦点缺省在第一个回答（投递）。
                let lit = async {
                    while color_of(&ui, "1  Deliver") != Some(theme.brand) {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                };
                tokio::time::timeout(std::time::Duration::from_secs(10), lit)
                    .await
                    .unwrap_or_else(|_| panic!("the answers never lit:\n{}", ui.screen()));
                let buffer = ui.frame();
                let area = buffer.area;
                let panel = theme.panel.expect("rich 的主题有板的底色");
                for needle in ["1  Deliver", "2  Deny"] {
                    let (_, y) = buffer.find(needle).unwrap();
                    for x in area.left()..area.right() {
                        assert_eq!(buffer[(x, y)].bg, panel, "column {x} of row {y}");
                    }
                }
                // 焦点行的序号与名字是 brand；其余序号 fg3、名字 fg2、说明 fg3（kit `answer_rows`）。
                assert_eq!(color_of(&ui, "1  Deliver"), Some(theme.brand));
                assert_eq!(color_of(&ui, "Deliver   the"), Some(theme.brand));
                assert_eq!(color_of(&ui, "2  Deny"), Some(theme.fg3.fg_color()));
                assert_eq!(color_of(&ui, "Deny      drop"), Some(theme.fg2.fg_color()));
                assert_eq!(color_of(&ui, "drop it and"), Some(theme.fg3.fg_color()));
                ui.key(KeyCode::Esc);
            },
        )
        .await;
    }
}
