//! 冷读取的“当前轮以错误结束”（session-lifecycle.md §6.3.1）：按当前轮输入的 `turn_usage.status`。

use rust_cli_kernel_api::ids::{CommandId, MessageId, OwnerEpoch, SessionId, TurnId};
use rust_cli_kernel_api::metering::CallStatus;
use rust_cli_kernel_api::store::{Fact, FactBatch, SessionStore, TurnAnchor};

use crate::tests::{TestStore, started, user};

fn batch(facts: Vec<Fact>, prompt: &str, turn: &str) -> FactBatch {
    FactBatch {
        owner_epoch: OwnerEpoch::LOCAL,
        command: None::<CommandId>,
        facts,
        turn: Some(TurnAnchor {
            prompt: MessageId::new(prompt),
            turn: Some(TurnId::new(turn)),
        }),
    }
}

#[tokio::test]
async fn a_turn_that_ended_with_an_error_is_reported_until_the_next_input() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let session = SessionId::new("sess_1");
    let ended = |status| Fact::TurnEnded { status };
    t.store
        .commit(
            &session,
            batch(vec![user("msg_u1", "one")], "msg_u1", "turn_1"),
        )
        .await
        .unwrap();
    assert!(!t.store.load(&session).await.unwrap().last_turn_failed);
    t.store
        .commit(
            &session,
            batch(vec![ended(CallStatus::Error)], "msg_u1", "turn_1"),
        )
        .await
        .unwrap();
    assert!(t.store.load(&session).await.unwrap().last_turn_failed);
    t.store
        .commit(
            &session,
            batch(vec![user("msg_u2", "two")], "msg_u2", "turn_2"),
        )
        .await
        .unwrap();
    assert!(!t.store.load(&session).await.unwrap().last_turn_failed);
    t.store
        .commit(
            &session,
            batch(vec![ended(CallStatus::Cancelled)], "msg_u2", "turn_2"),
        )
        .await
        .unwrap();
    assert!(!t.store.load(&session).await.unwrap().last_turn_failed);
}
