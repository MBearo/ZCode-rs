use std::sync::Arc;

use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::message::{Message, UserOrigin};
use rust_cli_kernel_api::session_api::{Ack, Command, SessionHandle};
use rust_cli_kernel_api::store::{Fact, SessionKind, SessionMeta, SessionStore};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};

use crate::test_support::{Setup, envelope, finished, submit, until};

/// fork：新会话（kind Fork、parent 原会话、Node 的 `Fork of <标题>`），消息换新 ID 复制。
#[tokio::test]
async fn forks_copy_the_active_branch_with_new_ids() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["hi"])]));
    let built = Setup::new(model).build().await;
    // 原会话标题取库里的会话行（Node `getSession`），不取 actor 内存里的空标题。
    let titled = SessionMeta {
        title: "Fix the login bug".to_owned(),
        ..crate::test_support::meta()
    };
    built.store.create(titled).await.unwrap();
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "  hello\n  world ")).await;
    until(&mut events, finished).await;
    let ack = built
        .session
        .send(envelope("f1", Command::Fork { at: None }))
        .await;
    let Ack::Accepted { result } = ack else {
        panic!("{ack:?}")
    };
    let child = SessionId::new(result.as_str().unwrap());
    let loaded = built.store.load(&child).await.unwrap();
    assert_eq!(loaded.meta.kind, SessionKind::Fork);
    assert_eq!(loaded.meta.parent, Some(crate::test_support::meta().id));
    assert_eq!(loaded.meta.title, "Fork of Fix the login bug");
    let original = built.session.view().await.unwrap().messages;
    assert_eq!(loaded.messages.len(), original.len());
    for (copy, source) in loaded.messages.iter().zip(&original) {
        assert_ne!(copy.id(), source.id());
    }
    assert!(matches!(&loaded.messages[0], Message::User(user) if user.origin == UserOrigin::Human));
    let facts = built.store.facts(&child);
    assert!(matches!(facts[0], Fact::ModelSelected { .. }));
}

/// 原会话行还没有标题（还没有人类输入）时用 Node `titleFromInput` 的缺省值。
#[tokio::test]
async fn a_fork_of_an_untitled_session_is_named_like_node() {
    let model = Arc::new(ScriptedModel::new(Vec::new()));
    let built = Setup::new(model).build().await;
    let ack = built
        .session
        .send(envelope("f1", Command::Fork { at: None }))
        .await;
    let Ack::Accepted { result } = ack else {
        panic!("{ack:?}")
    };
    let child = SessionId::new(result.as_str().unwrap());
    let loaded = built.store.load(&child).await.unwrap();
    assert_eq!(loaded.meta.title, "Fork of Untitled session");
}
