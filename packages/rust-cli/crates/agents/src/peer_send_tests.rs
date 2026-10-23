//! ListAgents 的同伴段与 SendMessage 的同伴寻址（cross-session.md §1.3、§1.4）：期望值是固定的原文。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::peers::{
    PeerAddress, PeerIdentity, PeerMessaging, PeerOutbound, PeerRecord, PeerSendError, PeerSignal,
    PeerStatus, ReceiptStatus, parse_envelope,
};
use serde_json::json;

use super::ago;
use crate::test_support::{text, with_peers};

#[derive(Default)]
struct FakePeers {
    peers: Vec<PeerRecord>,
    sent: Mutex<Vec<PeerOutbound>>,
    fail: bool,
}

const OWN: &str = "/peers/200.sock";

#[async_trait]
impl PeerMessaging for FakePeers {
    async fn identity(&self) -> Option<PeerIdentity> {
        Some(PeerIdentity {
            name: "work-b-7f".to_owned(),
            address: PeerAddress::from_socket(OWN),
        })
    }
    async fn peers(&self) -> Vec<PeerRecord> {
        self.peers.clone()
    }
    async fn send(&self, message: PeerOutbound) -> Result<(), PeerSendError> {
        self.sent.lock().unwrap().push(message);
        if self.fail {
            Err(PeerSendError::Gone)
        } else {
            Ok(())
        }
    }
    async fn receipt(&self, _to: PeerAddress, _msg_id: String, _status: ReceiptStatus) {}
    async fn bind(&self, _session: &SessionId) {}
    async fn unbind(&self, _session: &SessionId) {}
    fn signals(&self, _session: &SessionId) -> BoxStream<'static, PeerSignal> {
        Box::pin(futures_util::stream::pending())
    }
    async fn ack(&self, _session: &SessionId, _seq: u64) {}
    async fn set_status(&self, _session: &SessionId, _status: PeerStatus) {}
}

fn peer(pid: u32, name: &str) -> PeerRecord {
    PeerRecord {
        pid,
        name: name.to_owned(),
        address: PeerAddress::from_socket(&format!("/peers/{pid}.sock")),
        kind: "interactive".to_owned(),
        status: Some(PeerStatus::Busy),
        // 测试的时钟是 1000 ms：启动在 0 ms，`started 1s ago`。
        started_at_ms: 0,
    }
}

#[test]
fn ages_use_the_most_significant_unit() {
    assert_eq!(ago(0), "0s");
    assert_eq!(ago(3_400), "3s");
    assert_eq!(ago(125_000), "2m");
    assert_eq!(ago(7_200_000), "2h");
    assert_eq!(ago(90_000_000), "1d");
}

#[tokio::test]
async fn the_listing_has_this_session_and_the_peer_rows() {
    let fake = Arc::new(FakePeers {
        peers: vec![peer(100, "work-6d")],
        ..FakePeers::default()
    });
    let harness = with_peers(fake.clone());
    let listed = text(&harness.call("sess_b", "ListAgents", json!({})).await);
    let own = PeerAddress::from_socket(OWN).reference();
    let theirs = PeerAddress::from_socket("/peers/100.sock").reference();
    assert_eq!(
        listed,
        format!(
            "This session is work-b-7f [{}] — the name other sessions use to message it (it is not listed below; a message to it would be a message to yourself).\n\nPeer sessions (1):\n  work-6d [{}]  ·  interactive  ·  busy  ·  started 1s ago",
            &own[..6],
            &theirs[..6]
        )
    );
    // 没有同伴：自己那一段加“没有可达的 agent”。
    let alone = with_peers(Arc::new(FakePeers::default()));
    let listed = text(&alone.call("sess_b", "ListAgents", json!({})).await);
    assert!(listed.ends_with("\n\nNo reachable agents — no other ZCode session is running on this machine right now (peer messaging itself is available; a session appears here once it is started)."));
}

#[tokio::test]
async fn sending_by_name_wraps_the_message_in_an_envelope() {
    let fake = Arc::new(FakePeers {
        peers: vec![peer(100, "work-6d")],
        ..FakePeers::default()
    });
    let harness = with_peers(fake.clone());
    let output = harness
        .call(
            "sess_b",
            "SendMessage",
            json!({"to": "Work-6D", "summary": "say hello", "message": "hello from B"}),
        )
        .await;
    assert!(!output.is_error);
    assert_eq!(
        text(&output),
        "“say hello” → work-6d (another ZCode session on this machine; in that session's inbox, not yet read by its ZCode — a [Cross-session delivery notice] follows if that session holds it (usually a different permission mode) or refuses it)"
    );
    let sent = fake.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to.socket(), "/peers/100.sock");
    let envelope = parse_envelope(&sent[0].envelope).unwrap();
    assert_eq!(envelope.from.as_deref(), Some("uds:/peers/200.sock"));
    assert_eq!(envelope.from_name.as_deref(), Some("work-b-7f"));
    assert_eq!(envelope.body, "hello from B");
}

#[tokio::test]
async fn two_peers_with_one_name_need_the_ref() {
    let fake = Arc::new(FakePeers {
        peers: vec![peer(100, "work-6d"), peer(101, "work-6d")],
        ..FakePeers::default()
    });
    let harness = with_peers(fake.clone());
    let output = harness
        .call(
            "sess_b",
            "SendMessage",
            json!({"to": "work-6d", "message": "hi"}),
        )
        .await;
    assert!(text(&output).starts_with(
        "2 agents are named 'work-6d'. Re-send with the ref of the one you mean:\n  work-6d ["
    ));
    assert!(fake.sent.lock().unwrap().is_empty());
    let reference = PeerAddress::from_socket("/peers/101.sock").reference();
    let to = format!("work-6d [{}]", &reference[..6]);
    let output = harness
        .call("sess_b", "SendMessage", json!({"to": to, "message": "hi"}))
        .await;
    assert!(text(&output).contains(" → work-6d (another ZCode session"));
    assert_eq!(fake.sent.lock().unwrap()[0].to.socket(), "/peers/101.sock");
}

#[tokio::test]
async fn unknown_names_and_failed_sends_use_the_fixed_text() {
    let fake = Arc::new(FakePeers {
        peers: vec![peer(100, "work-6d")],
        fail: true,
        ..FakePeers::default()
    });
    let harness = with_peers(fake);
    let output = harness
        .call(
            "sess_b",
            "SendMessage",
            json!({"to": "nobody", "message": "hi"}),
        )
        .await;
    assert!(!output.is_error);
    assert_eq!(
        text(&output),
        "No agent named 'nobody' is reachable.\nUse ListAgents to see everyone you can message."
    );
    let output = harness
        .call(
            "sess_b",
            "SendMessage",
            json!({"to": "work-6d", "message": "hi"}),
        )
        .await;
    assert_eq!(
        text(&output),
        "Failed to send to work-6d — that session may have just exited. Call ListAgents to see who is reachable now."
    );
}
