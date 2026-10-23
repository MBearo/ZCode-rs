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
        clock: Arc::new(FixedClock(1_000)),
        ids: Arc::new(SequentialIds::default()),
        spawner: scope.spawner(),
    })
    .await
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
