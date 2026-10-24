//! 续写占位（`No response requested.`，docs/specs/rust-cli/node-store.md §4.2.1，工作线 F77）：写成 Node 只给模型看的
//! assistant（App 不显示），冷读取还原成占位；文字相同的普通回复照常是回复。
use rust_cli_kernel_api::ids::{MessageId, SessionId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, NO_RESPONSE_REQUESTED, SYNTHETIC_MODEL, StopReason,
};
use rust_cli_kernel_api::store::{Fact, SessionStore};
use serde_json::{Value, json};

use crate::tests::{TestStore, assistant, batch, started, text, user};

fn sid() -> SessionId {
    SessionId::new("sess_1")
}

fn placeholder(id: &str) -> Fact {
    Fact::MessageAppended {
        message: Message::Assistant(AssistantMessage {
            id: MessageId::new(id),
            content: vec![ContentBlock::Text {
                text: NO_RESPONSE_REQUESTED.to_owned(),
            }],
            model: SYNTHETIC_MODEL.to_owned(),
            stop_reason: Some(StopReason::StopSequence),
            usage: None,
        }),
    }
}

async fn commit(t: &TestStore, facts: Vec<Fact>) {
    t.store.commit(&sid(), batch(None, facts)).await.unwrap();
}

/// 被中断的第一轮，续写时占位与新输入同一批（会话 actor 的写法）。
async fn resumed(t: &TestStore) {
    started(t, "sess_1").await;
    commit(t, vec![user("msg_u1", "first question")]).await;
    commit(
        t,
        vec![placeholder("msg_a0"), user("msg_u2", "second question")],
    )
    .await;
}

fn message_data(t: &TestStore, id: &str) -> Value {
    let rows = t.rows(&format!("select data from message where id = '{id}'"));
    serde_json::from_str(rows[0][0].as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn the_placeholder_is_stored_for_the_model_only() {
    let t = TestStore::new();
    resumed(&t).await;
    let data = message_data(&t, "msg_a0");
    assert_eq!(data["role"], "assistant");
    assert_eq!(
        data["semantics"],
        json!({
            "origin": "agent_runtime",
            "kind": "assistant_response",
            "uiVisibility": "hidden",
            "providerVisibility": "visible",
            "transcriptVisibility": "hidden",
        })
    );
    // 父消息是被中断的输入；模型是会话当前的选择（App 的 fork 按它选模型）。
    assert_eq!(data["parentID"], "msg_u1");
    assert_eq!(data["modelId"], "m");
    assert_eq!(data["finish"], "stop");
    let texts = t.rows(
        "select json_extract(data, '$.text') from part
          where message_id = 'msg_a0' and json_extract(data, '$.type') = 'text'",
    );
    assert_eq!(texts, vec![vec![json!(NO_RESPONSE_REQUESTED)]]);
}

#[tokio::test]
async fn the_placeholder_reads_back_where_it_was_written() {
    let t = TestStore::new();
    resumed(&t).await;
    let loaded = t.store.load(&sid()).await.unwrap();
    let ids: Vec<&str> = loaded.messages.iter().map(|m| m.id().as_str()).collect();
    assert_eq!(ids, ["msg_u1", "msg_a0", "msg_u2"]);
    let Message::Assistant(read) = &loaded.messages[1] else {
        panic!("{:?}", loaded.messages[1]);
    };
    assert!(read.is_resume_sentinel(), "{read:?}");
    assert_eq!(read.stop_reason, Some(StopReason::StopSequence));
    assert_eq!(read.usage, None);
}

#[tokio::test]
async fn a_visible_reply_with_the_same_text_stays_a_reply() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    commit(&t, vec![user("msg_u1", "say it")]).await;
    commit(
        &t,
        vec![assistant("msg_a1", vec![text(NO_RESPONSE_REQUESTED)], None)],
    )
    .await;
    assert_eq!(
        message_data(&t, "msg_a1")["semantics"]["uiVisibility"],
        "visible"
    );
    let loaded = t.store.load(&sid()).await.unwrap();
    let Message::Assistant(read) = &loaded.messages[1] else {
        panic!("{:?}", loaded.messages[1]);
    };
    assert!(!read.is_resume_sentinel());
    assert_eq!(read.model, "m");
}

/// 会话里切过去时占位单独一批（工作线 F79）：父消息是被中断的输入，读回时是最后一条、仍是占位，当前轮不算失败。
#[tokio::test]
async fn a_placeholder_written_alone_reads_back_at_the_end() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    commit(&t, vec![user("msg_u1", "first question")]).await;
    commit(&t, vec![placeholder("msg_a0")]).await;
    let data = message_data(&t, "msg_a0");
    assert_eq!(data["parentID"], "msg_u1");
    assert_eq!(data["semantics"]["uiVisibility"], "hidden");
    let loaded = t.store.load(&sid()).await.unwrap();
    let ids: Vec<&str> = loaded.messages.iter().map(|m| m.id().as_str()).collect();
    assert_eq!(ids, ["msg_u1", "msg_a0"]);
    assert!(
        matches!(&loaded.messages[1], Message::Assistant(read) if read.is_resume_sentinel()),
        "{:?}",
        loaded.messages[1]
    );
    assert!(!loaded.last_turn_failed);
}
