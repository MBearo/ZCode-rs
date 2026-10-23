//! 会话 actor 的跨会话入站（cross-session.md §2.4、§3）：接收的在空闲时开回合，模式不同时扣住、回执，批准后投递，
//! 拒绝、改模式与过期，重开会话后扣住的仍在。端口是测试替身：入站经通道推入，回执与 ack 记下来。

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::message::{ContentBlock, Message, UserOrigin};
use rust_cli_kernel_api::peers::{
    InboundPeerMessage, ModeClass, PeerAddress, PeerIdentity, PeerMessaging, PeerOutbound,
    PeerReceipt, PeerRecord, PeerSendError, PeerSignal, PeerStatus, ReceiptStatus, envelope,
};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{Command, KernelEvent, SessionHandle};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use tokio::sync::mpsc;

use crate::PeerDeps;
use crate::test_support::{Setup, envelope as command, finished, until};

#[derive(Default)]
struct FakePort {
    inbound: Mutex<Option<mpsc::UnboundedReceiver<PeerSignal>>>,
    sender: Mutex<Option<mpsc::UnboundedSender<PeerSignal>>>,
    receipts: Mutex<Vec<(String, ReceiptStatus)>>,
    acked: Mutex<Vec<u64>>,
    bound: Mutex<Vec<String>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl FakePort {
    fn new() -> Arc<Self> {
        let port = Arc::new(Self::default());
        port.reset();
        port
    }

    /// 新的入站通道（重开会话时 actor 重新订阅）。
    fn reset(&self) {
        let (sender, receiver) = mpsc::unbounded_channel();
        *lock(&self.sender) = Some(sender);
        *lock(&self.inbound) = Some(receiver);
    }

    fn push(&self, signal: PeerSignal) {
        lock(&self.sender).as_ref().unwrap().send(signal).unwrap();
    }

    fn receipts(&self) -> Vec<(String, ReceiptStatus)> {
        lock(&self.receipts).clone()
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
    async fn receipt(&self, _to: PeerAddress, msg_id: String, status: ReceiptStatus) {
        lock(&self.receipts).push((msg_id, status));
    }
    async fn bind(&self, session: &SessionId) {
        lock(&self.bound).push(session.to_string());
    }
    async fn unbind(&self, _session: &SessionId) {}
    fn signals(&self, _session: &SessionId) -> BoxStream<'static, PeerSignal> {
        let receiver = lock(&self.inbound).take().unwrap();
        Box::pin(futures_util::stream::unfold(
            receiver,
            |mut receiver| async move { receiver.recv().await.map(|signal| (signal, receiver)) },
        ))
    }
    async fn ack(&self, _session: &SessionId, seq: u64) {
        lock(&self.acked).push(seq);
    }
    async fn set_status(&self, _session: &SessionId, _status: PeerStatus) {}
}

const SENDER: &str = "/peers/9.sock";

fn message(seq: u64, msg_id: &str, mode: ModeClass, received_at_ms: u64) -> PeerSignal {
    let from = PeerAddress::from_socket(SENDER);
    PeerSignal::Message(InboundPeerMessage {
        seq,
        msg_id: msg_id.to_owned(),
        envelope: envelope(&from, Some("work-b-7f"), mode, "hello from B"),
        from,
        received_at_ms,
    })
}

fn deps(port: &Arc<FakePort>, timeout: u64, wake: bool) -> PeerDeps {
    PeerDeps {
        port: port.clone(),
        hold_timeout_ms: timeout,
        receipts_wake: wake,
    }
}

fn user_texts(messages: &[Message]) -> Vec<(String, String)> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user) => {
                let source = match &user.origin {
                    UserOrigin::Meta { source } => source.clone(),
                    _ => String::new(),
                };
                let text = user
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                Some((source, text))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn an_accepted_message_opens_an_idle_turn_with_the_idle_wrap() {
    let port = FakePort::new();
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(vec![text_response(&["ok"])])));
    setup.peers = deps(&port, 300_000, true);
    let built = setup.build().await;
    let mut events = built.session.subscribe(None);
    port.push(message(1, "m1", ModeClass::Prompting, 0));
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    let users = user_texts(&view.messages);
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].0, "cross-session");
    assert!(users[0].1.starts_with("Another ZCode session sent a message:\n<cross-session-message from=\"uds:/peers/9.sock\" from-name=\"work-b-7f\" from-mode=\"prompting\">\nhello from B\n</cross-session-message>"));
    // 先提交再 ack；主会话已绑定。
    assert_eq!(*lock(&port.acked), [1]);
    assert_eq!(*lock(&port.bound), ["sess_1"]);
    assert!(port.receipts().is_empty());
    let entries = built.store.facts(&SessionId::new("sess_1"));
    assert!(entries.iter().any(|fact| matches!(fact, rust_cli_kernel_api::store::Fact::Entry { kind, id, .. } if kind == "peer_message" && id == "m1")));
}

#[tokio::test]
async fn a_mismatched_message_is_held_until_approved() {
    let port = FakePort::new();
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(vec![text_response(&["ok"])])));
    setup.peers = deps(&port, 300_000, true);
    let built = setup.build().await;
    let mut events = built.session.subscribe(None);
    port.push(message(1, "m1", ModeClass::Bypass, 0));
    let seen = until(&mut events, |event| {
        matches!(event, KernelEvent::PeerHeldChanged { .. })
    })
    .await;
    let Some(KernelEvent::PeerHeldChanged { held }) = seen.last() else {
        panic!("expected the held snapshot");
    };
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].body, "hello from B");
    assert_eq!(held[0].from_name.as_deref(), Some("work-b-7f"));
    assert_eq!(held[0].expires_at_ms, 300_000);
    let view = built.session.view().await.unwrap();
    assert_eq!(view.peer_held.len(), 1);
    assert!(user_texts(&view.messages).is_empty());
    assert_eq!(port.receipts(), [("m1".to_owned(), ReceiptStatus::Held)]);

    let ack = built
        .session
        .send(command(
            "c1",
            Command::ResolvePeerMessage {
                message: "m1".to_owned(),
                approve: true,
            },
        ))
        .await;
    assert!(
        matches!(ack, rust_cli_kernel_api::session_api::Ack::Accepted { .. }),
        "{ack:?}"
    );
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    assert!(view.peer_held.is_empty());
    let users = user_texts(&view.messages);
    assert_eq!(users.len(), 1);
    assert!(users[0].1.contains("from-mode=\"bypass\">\nhello from B\n"));
    assert_eq!(
        port.receipts(),
        [
            ("m1".to_owned(), ReceiptStatus::Held),
            ("m1".to_owned(), ReceiptStatus::Delivered)
        ]
    );
    // 已解决的再解决：拒绝。
    let again = built
        .session
        .send(command(
            "c2",
            Command::ResolvePeerMessage {
                message: "m1".to_owned(),
                approve: false,
            },
        ))
        .await;
    assert!(matches!(
        again,
        rust_cli_kernel_api::session_api::Ack::Rejected { .. }
    ));
}

#[tokio::test]
async fn denial_mode_changes_and_expiry_settle_held_messages() {
    let port = FakePort::new();
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(vec![text_response(&["ok"])])));
    setup.peers = deps(&port, 300_000, true);
    let built = setup.build().await;
    let mut events = built.session.subscribe(None);
    port.push(message(1, "m1", ModeClass::Bypass, 0));
    port.push(message(2, "m2", ModeClass::Bypass, 0));
    until(
        &mut events,
        |event| matches!(event, KernelEvent::PeerHeldChanged { held } if held.len() == 2),
    )
    .await;
    built
        .session
        .send(command(
            "c1",
            Command::ResolvePeerMessage {
                message: "m1".to_owned(),
                approve: false,
            },
        ))
        .await;
    // 切到 bypass：类别相同了，m2 放行并投递。
    built
        .session
        .send(command(
            "c2",
            Command::SetMode {
                mode: PermissionMode::BypassPermissions,
            },
        ))
        .await;
    until(&mut events, finished).await;
    assert_eq!(
        port.receipts(),
        [
            ("m1".to_owned(), ReceiptStatus::Held),
            ("m2".to_owned(), ReceiptStatus::Held),
            ("m1".to_owned(), ReceiptStatus::Denied),
            ("m2".to_owned(), ReceiptStatus::Delivered),
        ]
    );

    // 过期：时钟是 1000 ms，收到于 0、时长 500 的消息到期。
    let port = FakePort::new();
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(Vec::new())));
    setup.peers = deps(&port, 500, true);
    let built = setup.build().await;
    let mut events = built.session.subscribe(None);
    port.push(message(1, "m3", ModeClass::Bypass, 0));
    until(
        &mut events,
        |event| matches!(event, KernelEvent::PeerHeldChanged { held } if held.is_empty()),
    )
    .await;
    assert_eq!(
        port.receipts(),
        [
            ("m3".to_owned(), ReceiptStatus::Held),
            ("m3".to_owned(), ReceiptStatus::Expired)
        ]
    );
}

#[tokio::test]
async fn a_held_message_is_restored_when_the_session_reopens() {
    let port = FakePort::new();
    let store = Arc::new(rust_cli_kernel_api::testing::MemoryStore::default());
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(Vec::new())));
    setup.peers = deps(&port, 300_000, true);
    setup.store = Some(store.clone());
    {
        use rust_cli_kernel_api::store::SessionStore;
        store.create(crate::test_support::meta()).await.unwrap();
    }
    let built = setup.build().await;
    let mut events = built.session.subscribe(None);
    port.push(message(1, "m1", ModeClass::Bypass, 0));
    until(&mut events, |event| {
        matches!(event, KernelEvent::PeerHeldChanged { .. })
    })
    .await;
    built
        .session
        .close(rust_cli_kernel_api::session_api::EndReason::Other)
        .await;
    drop(built);

    // 同一个存储重开：扣住的仍在，不再回执 held。
    port.reset();
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(Vec::new())));
    setup.peers = deps(&port, 300_000, true);
    setup.store = Some(store);
    setup.opened = crate::Opened::Loaded;
    let built = setup.build().await;
    let view = built.session.view().await.unwrap();
    assert_eq!(view.peer_held.len(), 1);
    assert_eq!(view.peer_held[0].msg_id, "m1");
    assert_eq!(port.receipts(), [("m1".to_owned(), ReceiptStatus::Held)]);
    // 同一条再送一次（对方没收到 ack 时重发）：只 ack，不再扣一次。
    port.push(message(5, "m1", ModeClass::Bypass, 0));
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(built.session.view().await.unwrap().peer_held.len(), 1);
    assert!(lock(&port.acked).contains(&5));
}

#[tokio::test]
async fn receipts_wait_for_a_tool_boundary_when_they_do_not_wake() {
    let port = FakePort::new();
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(vec![text_response(&["ok"])])));
    setup.peers = deps(&port, 300_000, false);
    let built = setup.build().await;
    port.push(PeerSignal::Receipt(PeerReceipt {
        seq: 1,
        msg_id: "m9".to_owned(),
        status: ReceiptStatus::Held,
        recipient: PeerAddress::from_socket("/peers/1.sock"),
    }));
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let view = built.session.view().await.unwrap();
    // `-p`（passive）：不开回合，留到下一个回合的工具边界。
    assert!(!view.running);
    assert!(user_texts(&view.messages).is_empty());
    assert_eq!(*lock(&port.acked), [1]);
}

#[tokio::test]
async fn a_zero_timeout_never_expires() {
    // 过期时长为 0（设置 `never`）不计时。
    let port = FakePort::new();
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(Vec::new())));
    setup.peers = deps(&port, 0, true);
    let built = setup.build().await;
    let mut events = built.session.subscribe(None);
    port.push(message(1, "m1", ModeClass::Bypass, 0));
    until(&mut events, |event| {
        matches!(event, KernelEvent::PeerHeldChanged { .. })
    })
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let view = built.session.view().await.unwrap();
    assert_eq!(view.peer_held.len(), 1);
    assert_eq!(view.peer_held[0].expires_at_ms, u64::MAX);
    assert_eq!(port.receipts(), [("m1".to_owned(), ReceiptStatus::Held)]);
}
