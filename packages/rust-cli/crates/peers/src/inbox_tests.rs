//! 入站表：绑定前不交出、ack 后回复、解绑后交给下一个会话；回执只认发出去的消息（cross-session.md §2.1）。

use futures_util::StreamExt;
use rust_cli_kernel_api::peers::{InboundPeerMessage, PeerAddress, PeerSignal, ReceiptStatus};

use super::*;

fn message(seq: u64) -> PeerSignal {
    PeerSignal::Message(InboundPeerMessage {
        seq,
        msg_id: format!("m{seq}"),
        from: PeerAddress::from_socket("/s.sock"),
        envelope: "x".to_owned(),
        received_at_ms: 0,
    })
}

#[tokio::test]
async fn signals_wait_for_the_bound_session_and_ack_releases_the_sender() {
    let inbox = Inbox::new();
    let (one, two) = (SessionId::new("sess_1"), SessionId::new("sess_2"));
    let wait = inbox.push(message).unwrap();
    let mut first = inbox.subscribe(&one);
    inbox.bind(&one);
    assert_eq!(first.next().await.unwrap().seq(), 1);
    // 第一个会话没 ack 就关闭：下一个会话重新拿到它（至少一次）。
    inbox.unbind(&one);
    let mut second = inbox.subscribe(&two);
    inbox.bind(&two);
    assert_eq!(second.next().await.unwrap().seq(), 1);
    inbox.ack(1);
    wait.await.unwrap();
}

#[test]
fn receipts_match_only_outstanding_sends() {
    let inbox = Inbox::new();
    let to = PeerAddress::from_socket("/peer.sock");
    assert_eq!(inbox.match_receipt("m1", ReceiptStatus::Held), None);
    inbox.record_send("m1", &to);
    assert_eq!(
        inbox.match_receipt("m1", ReceiptStatus::Held),
        Some(to.clone())
    );
    // held 之后只认终态。
    assert_eq!(inbox.match_receipt("m1", ReceiptStatus::Held), None);
    assert_eq!(inbox.match_receipt("m1", ReceiptStatus::Expired), Some(to));
    assert_eq!(inbox.match_receipt("m1", ReceiptStatus::Expired), None);
}

#[test]
fn a_closed_inbox_takes_nothing() {
    let inbox = Inbox::new();
    inbox.close();
    assert!(inbox.push(message).is_none());
}
