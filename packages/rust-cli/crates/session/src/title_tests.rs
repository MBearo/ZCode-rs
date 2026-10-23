//! 会话标题的触发、时序与改名（titles.md §2、§4）：真实的会话 actor 与主循环，脚本模型。
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::message::UserMessage;
use rust_cli_kernel_api::model::{ModelError, ModelEvent, ModelPort, ModelRequest};
use rust_cli_kernel_api::session_api::{Command, SessionHandle};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use tokio::sync::Notify;

use super::*;
use crate::TitleModel;
use crate::test_support::{Setup, envelope, finished, submit, until};

fn titles(model: Arc<dyn ModelPort>) -> Option<TitleModel> {
    Some(TitleModel {
        model,
        directory: None,
    })
}

fn titled(event: &KernelEvent, expected: &str) -> bool {
    matches!(event, KernelEvent::TitleChanged { title } if title == expected)
}

fn generated(facts: &[Fact]) -> Vec<Fact> {
    facts
        .iter()
        .filter(|fact| matches!(fact, Fact::TitleGenerated { .. }))
        .cloned()
        .collect()
}

#[tokio::test]
async fn the_first_input_title_comes_first_then_the_generated_title() {
    let main = Arc::new(ScriptedModel::new(vec![text_response(&["Sure."])]));
    let title_model = Arc::new(ScriptedModel::new(vec![text_response(&[
        "{\"title\":\"Hook trust digest\"}",
    ])]));
    let mut setup = Setup::new(main.clone());
    setup.titles = titles(title_model.clone());
    let built = setup.build().await;
    let mut events = built.session.subscribe(None);
    built
        .session
        .send(submit("c1", "Explain the hook trust digest"))
        .await;
    let seen = until(&mut events, |event| titled(event, "Hook trust digest")).await;
    let first = seen
        .iter()
        .position(|event| titled(event, "Explain the hook trust digest"))
        .expect("first_input title");
    let done = seen
        .iter()
        .position(|event| titled(event, "Hook trust digest"))
        .unwrap();
    assert!(first < done);
    let facts = generated(
        &built
            .store
            .facts(&rust_cli_kernel_api::ids::SessionId::new("sess_1")),
    );
    let [Fact::TitleGenerated { title, message }] = facts.as_slice() else {
        panic!("one generated title expected: {facts:?}");
    };
    assert_eq!(title, "Hook trust digest");
    assert!(message.is_some());
    assert_eq!(
        built.session.view().await.unwrap().meta.title,
        "Hook trust digest"
    );
    assert_eq!(title_model.requests().len(), 1);
    // 工作线 F64：标题请求与提交这条输入的回合是同一个 `options.run`（每请求头的 `x-query-id`，model.md §2.4）。
    let turn = main.requests()[0].options.run.clone();
    assert!(turn.is_some());
    assert_eq!(title_model.requests()[0].options.run, turn);
}

#[tokio::test]
async fn short_inputs_and_later_turns_do_not_generate() {
    let main = Arc::new(ScriptedModel::new(vec![
        text_response(&["a"]),
        text_response(&["b"]),
    ]));
    let title_model = Arc::new(ScriptedModel::new(Vec::new()));
    let mut setup = Setup::new(main);
    setup.titles = titles(title_model.clone());
    let built = setup.build().await;
    let mut events = built.session.subscribe(None);
    built.session.send(submit("c1", "hi")).await;
    let seen = until(&mut events, finished).await;
    assert!(seen.iter().any(|event| titled(event, "hi")));
    // 第二个回合即使够长也不生成（每个会话只在首个回合尝试一次）。
    built
        .session
        .send(submit("c2", "now a much longer second message"))
        .await;
    until(&mut events, finished).await;
    assert!(title_model.requests().is_empty());

    // 已有 user 消息的会话（resume）不生成。
    let main = Arc::new(ScriptedModel::new(vec![text_response(&["c"])]));
    let mut setup = Setup::new(main);
    setup.titles = titles(title_model.clone());
    setup.messages = vec![Message::User(UserMessage {
        id: MessageId::new("msg_old"),
        content: vec![ContentBlock::Text {
            text: "earlier".to_owned(),
        }],
        origin: UserOrigin::Human,
    })];
    let built = setup.build().await;
    let mut events = built.session.subscribe(None);
    built
        .session
        .send(submit("c3", "a long enough follow up message"))
        .await;
    until(&mut events, finished).await;
    assert!(title_model.requests().is_empty());
}

/// 等放行才回复的标题模型。
struct Gated {
    open: Arc<Notify>,
    served: Arc<AtomicUsize>,
}

impl ModelPort for Gated {
    fn stream(
        &self,
        _request: ModelRequest,
        _cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
        let (open, served) = (self.open.clone(), self.served.clone());
        stream::once(async move {
            open.notified().await;
            served.fetch_add(1, Ordering::SeqCst);
            stream::iter(text_response(&["Generated name"]))
        })
        .flatten()
        .boxed()
    }
}

#[tokio::test]
async fn a_rename_before_the_title_arrives_wins() {
    let main = Arc::new(ScriptedModel::new(vec![text_response(&["ok"])]));
    let open = Arc::new(Notify::new());
    let served = Arc::new(AtomicUsize::new(0));
    let mut setup = Setup::new(main);
    setup.titles = titles(Arc::new(Gated {
        open: open.clone(),
        served: served.clone(),
    }));
    let built = setup.build().await;
    let mut events = built.session.subscribe(None);
    built
        .session
        .send(submit("c1", "please name this session later"))
        .await;
    until(&mut events, finished).await;
    let ack = built
        .session
        .send(envelope(
            "c2",
            Command::Rename {
                title: "Mine".to_owned(),
            },
        ))
        .await;
    assert!(matches!(
        ack,
        rust_cli_kernel_api::session_api::Ack::Accepted { .. }
    ));
    until(&mut events, |event| titled(event, "Mine")).await;
    open.notify_one();
    while served.load(Ordering::SeqCst) == 0 {
        tokio::task::yield_now().await;
    }
    for _ in 0..20 {
        assert_eq!(built.session.view().await.unwrap().meta.title, "Mine");
    }
    let facts = built
        .store
        .facts(&rust_cli_kernel_api::ids::SessionId::new("sess_1"));
    assert!(generated(&facts).is_empty());
    assert!(
        facts
            .iter()
            .any(|fact| matches!(fact, Fact::TitleChanged { title } if title == "Mine"))
    );
}
