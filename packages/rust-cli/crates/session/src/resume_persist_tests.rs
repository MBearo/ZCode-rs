//! 续写占位落库（docs/specs/rust-cli/session-lifecycle.md §6.3.1，工作线 F77、F79）：打开时补在内存里，随第一批带消息的
//! 提交写进存储，排在这一批最前面；只打开不续写时不写。会话里切过去时（`Command::StoreResumeSentinel`）马上单独写一批。
use std::sync::Arc;

use rust_cli_kernel_api::ids::{MessageId, OwnerEpoch, SessionId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, StopReason, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::session_api::{Ack, Command, EndReason, SessionHandle};
use rust_cli_kernel_api::store::{Fact, FactBatch, SessionStore};
use rust_cli_kernel_api::testing::{MemoryStore, ScriptedModel, text_response};

use crate::Opened;
use crate::test_support::{Setup, envelope, finished, meta, submit, until};

fn human(id: &str, text: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        origin: UserOrigin::Human,
    })
}

fn answer(id: &str, text: &str) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        model: "m".to_owned(),
        stop_reason: Some(StopReason::EndTurn),
        usage: None,
    })
}

/// 存储里放好 `history`，会话从存储打开。
async fn opened(history: Vec<Message>) -> crate::test_support::Built {
    let store = Arc::new(MemoryStore::default());
    store.create(meta()).await.unwrap();
    let batch = FactBatch {
        owner_epoch: OwnerEpoch::LOCAL,
        command: None,
        facts: history
            .iter()
            .map(|message| Fact::MessageAppended {
                message: message.clone(),
            })
            .collect(),
        turn: None,
    };
    store.commit(&meta().id, batch).await.unwrap();
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(vec![
        text_response(&["second answer"]),
        text_response(&["third answer"]),
    ])));
    setup.store = Some(store);
    setup.messages = history;
    setup.opened = Opened::Loaded;
    setup.build().await
}

/// 存储里有一轮被中断的输入（没有回答），会话从存储打开。
async fn resumed() -> crate::test_support::Built {
    opened(vec![human("msg_u1", "first question")]).await
}

async fn store_now(built: &crate::test_support::Built, id: &str) -> Ack {
    built
        .session
        .send(envelope(id, Command::StoreResumeSentinel))
        .await
}

/// 存储里的消息：续写占位记作 `S`，其余是消息 ID。
fn stored(store: &MemoryStore) -> Vec<String> {
    store
        .facts(&SessionId::new("sess_1"))
        .iter()
        .filter_map(|fact| match fact {
            Fact::MessageAppended {
                message: Message::Assistant(assistant),
            } if assistant.is_resume_sentinel() => Some("S".to_owned()),
            Fact::MessageAppended { message } => Some(message.id().as_str().to_owned()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn the_reply_is_stored_with_the_next_input() {
    let built = resumed().await;
    let mut events = built.session.subscribe(None);
    built.session.send(submit("c1", "second question")).await;
    until(&mut events, finished).await;
    // 原来的输入、占位、新输入、回答：占位排在新输入之前（同一批的最前面）。
    let first = stored(&built.store);
    assert_eq!(first[..2], ["msg_u1", "S"], "{first:?}");
    assert_eq!(first.len(), 4, "{first:?}");
    assert!(first[2..].iter().all(|id| id != "S"), "{first:?}");
    // 之后的回合不再写。
    built.session.send(submit("c2", "third question")).await;
    until(&mut events, finished).await;
    let second = stored(&built.store);
    assert_eq!(second.len(), 6, "{second:?}");
    assert_eq!(second.iter().filter(|id| *id == "S").count(), 1);
}

#[tokio::test]
async fn opening_without_continuing_stores_nothing() {
    let built = resumed().await;
    let view = built.session.view().await.unwrap().messages;
    assert!(
        matches!(view.last(), Some(Message::Assistant(a)) if a.is_resume_sentinel()),
        "{view:?}"
    );
    built.session.close(EndReason::Other).await;
    assert_eq!(stored(&built.store), ["msg_u1"]);
}

/// 会话里切到这个会话之后（工作线 F79）：命令马上把占位单独写进存储；之后的输入不再带它。
#[tokio::test]
async fn the_store_command_writes_the_reply_at_once() {
    let built = resumed().await;
    let ack = store_now(&built, "c0").await;
    assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
    assert_eq!(stored(&built.store), ["msg_u1", "S"]);
    let mut events = built.session.subscribe(None);
    built.session.send(submit("c1", "second question")).await;
    until(&mut events, finished).await;
    let after = stored(&built.store);
    assert_eq!(after.len(), 4, "{after:?}");
    assert_eq!(after[..2], ["msg_u1", "S"], "{after:?}");
    assert_eq!(after.iter().filter(|id| *id == "S").count(), 1, "{after:?}");
}

#[tokio::test]
async fn the_store_command_twice_writes_it_once() {
    let built = resumed().await;
    for id in ["c0", "c1"] {
        let ack = store_now(&built, id).await;
        assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
    }
    built.session.close(EndReason::Other).await;
    assert_eq!(stored(&built.store), ["msg_u1", "S"]);
}

/// 没有补的会话（最后一条是回答）：命令什么都不做，回执照样是 `Accepted`。
#[tokio::test]
async fn the_store_command_without_a_reply_writes_nothing() {
    let built = opened(vec![
        human("msg_u1", "first question"),
        answer("msg_a1", "first answer"),
    ])
    .await;
    let ack = store_now(&built, "c0").await;
    assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
    assert_eq!(stored(&built.store), ["msg_u1", "msg_a1"]);
}
