//! 两个进程内的实例共用一个登记目录：发现、发送（对方会话提交后才成功）、回执、退出后删掉登记（cross-session.md
//! §2.1、§2.2）。

use std::sync::Arc;

use futures_util::StreamExt;
use rust_cli_base::TaskScope;
use rust_cli_kernel_api::peers::{
    ModeClass, PeerMessaging, PeerOutbound, PeerSignal, ReceiptStatus, envelope,
};
use rust_cli_kernel_api::testing::{FixedClock, SequentialIds};

use super::*;

async fn start(scope: &TaskScope, dir: &std::path::Path, pid: u32, cwd: &str) -> LocalPeers {
    LocalPeers::start(PeerConfig {
        registry: dir.join("peers"),
        fallback_sockets: vec![dir.join("fallback")],
        pid,
        cwd: cwd.to_owned(),
        version: "0.1.0".to_owned(),
        tmux: None,
        clock: Arc::new(FixedClock(1_000)),
        ids: Arc::new(SequentialIds::default()),
        spawner: scope.spawner(),
    })
    .await
}

/// 现场、tmux 与 `waiting` 进登记并读回；`stop_turn` 帧到达绑定的会话（tui-cockpit.md §4、§1.3）。
#[tokio::test]
async fn presence_tmux_and_waiting_round_trip_and_stop_reaches_the_bound_session() {
    let dir = tempfile::tempdir().unwrap();
    let scope = TaskScope::root("peers-test", |_| {});
    let place = TmuxPlace {
        socket: "/tmp/tmux-501/default".to_owned(),
        pane: "%12".to_owned(),
    };
    let a = LocalPeers::start(PeerConfig {
        registry: dir.path().join("peers"),
        fallback_sockets: vec![dir.path().join("fallback")],
        pid: std::process::id(),
        cwd: "/work/proj".to_owned(),
        version: "0.1.0".to_owned(),
        tmux: Some(place.clone()),
        clock: Arc::new(FixedClock(1_000)),
        ids: Arc::new(SequentialIds::default()),
        spawner: scope.spawner(),
    })
    .await;
    let b = start(&scope, dir.path(), 1, "/work/other").await;
    let session = SessionId::new("sess_a");
    a.bind(&session).await;
    let mut inbound = a.signals(&session);
    let presence = PeerPresence {
        now: Some("Bash  pnpm test".to_owned()),
        todos: Some((5, 8)),
        ..PeerPresence::default()
    };
    a.set_presence(&session, presence.clone()).await;
    a.set_status(&session, PeerStatus::Waiting).await;
    // 没绑定的会话写不进去。
    a.set_presence(&SessionId::new("other"), PeerPresence::default())
        .await;
    // 写登记在自己的任务里（`watch` 合并）：等到写完。
    let seen = loop {
        let seen = b.peers().await;
        if seen[0].status == Some(PeerStatus::Waiting) && seen[0].presence == presence {
            break seen;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    };
    assert_eq!(seen[0].session, Some(session.clone()));
    assert_eq!(seen[0].status, Some(PeerStatus::Waiting));
    assert_eq!(seen[0].presence, presence);
    assert_eq!(seen[0].tmux, Some(place));
    assert_eq!(
        a.own().await.and_then(|own| own.session),
        Some(session.clone())
    );

    let stop = b.stop(&seen[0].address);
    let receive = async {
        let PeerSignal::Stop { seq } = inbound.next().await.unwrap() else {
            panic!("expected a stop");
        };
        a.ack(&session, seq).await;
    };
    let (stopped, ()) = tokio::join!(stop, receive);
    stopped.unwrap();
}

#[tokio::test]
async fn two_instances_find_each_other_and_exchange_a_message_and_a_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let scope = TaskScope::root("peers-test", |_| {});
    // 用本测试进程的 pid 当 A（存活检查要真实的 pid），B 用一个假 pid。
    let a = start(&scope, dir.path(), std::process::id(), "/work/proj").await;
    let b = start(&scope, dir.path(), 1, "/work/other dir").await;
    let a_id = a.identity().await.unwrap();
    assert!(a_id.name.starts_with("proj-"), "{}", a_id.name);
    // B 看得到 A（pid 1 是 init / launchd，存活检查也认它，B 的登记同样会被 A 列出）。
    let seen = b.peers().await;
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].name, a_id.name);
    assert_eq!(seen[0].kind, "interactive");

    let session = SessionId::new("sess_a");
    a.bind(&session).await;
    let mut inbound = a.signals(&session);
    let b_id = b.identity().await.unwrap();
    let text = envelope(&b_id.address, Some(&b_id.name), ModeClass::Prompting, "hi");
    let send = b.send(PeerOutbound {
        to: a_id.address.clone(),
        msg_id: "m1".to_owned(),
        recipient_name: a_id.name.clone(),
        envelope: text.clone(),
    });
    let receive = async {
        let PeerSignal::Message(message) = inbound.next().await.unwrap() else {
            panic!("expected a message");
        };
        assert_eq!(message.envelope, text);
        assert_eq!(message.from, b_id.address);
        // 提交之后才 ack：发送方这时才成功。
        a.ack(&session, message.seq).await;
    };
    let (sent, ()) = tokio::join!(send, receive);
    sent.unwrap();

    // A 回执 held 给 B：B 绑定的会话收到。
    let b_session = SessionId::new("sess_b");
    b.bind(&b_session).await;
    let mut receipts = b.signals(&b_session);
    a.receipt(b_id.address.clone(), "m1".to_owned(), ReceiptStatus::Held)
        .await;
    let PeerSignal::Receipt(receipt) = receipts.next().await.unwrap() else {
        panic!("expected a receipt");
    };
    assert_eq!(receipt.status, ReceiptStatus::Held);
    assert_eq!(receipt.recipient, a_id.address);
    b.ack(&b_session, receipt.seq).await;

    a.shutdown().await;
    assert!(b.peers().await.is_empty());
    // 发给已退出的一方：连不上。
    let gone = b
        .send(PeerOutbound {
            to: a_id.address,
            msg_id: "m2".to_owned(),
            recipient_name: a_id.name,
            envelope: text,
        })
        .await;
    assert_eq!(gone, Err(PeerSendError::Gone));
    b.shutdown().await;
}
