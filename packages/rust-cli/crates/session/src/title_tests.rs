//! 会话标题的触发、时序与改名（titles.md §2、§4）：真实的会话 actor 与主循环，脚本模型。
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::message::{StopReason, Usage, UserMessage};
use rust_cli_kernel_api::metering::{CallStatus, ModelCall};
use rust_cli_kernel_api::model::{
    ModelError, ModelEvent, ModelPort, ModelRequest, QuerySource, SIDE_SESSION_TITLE,
};
use rust_cli_kernel_api::session_api::{Command, SessionHandle};
use rust_cli_kernel_api::testing::{MemoryStore, ScriptedModel, text_response};
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

/// 标题请求的用量事实（工作线 F73，usage.md §2.1.1）。
fn title_calls(facts: &[Fact]) -> Vec<ModelCall> {
    facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::ModelCalled { call }
                if call.source == QuerySource::Side(SIDE_SESSION_TITLE.to_owned()) =>
            {
                Some(call.clone())
            }
            _ => None,
        })
        .collect()
}

/// 等标题请求的用量事实提交（失败的请求没有界面事件可等）。
async fn until_title_call(store: &MemoryStore) -> ModelCall {
    let session = SessionId::new("sess_1");
    for _ in 0..500 {
        if let Some(call) = title_calls(&store.facts(&session)).pop() {
            return call;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("no usage row for the title request");
}

/// 带用量的文本响应：开头的输入用量与结尾的输出用量。
fn with_usage(
    mut events: Vec<Result<ModelEvent, ModelError>>,
) -> Vec<Result<ModelEvent, ModelError>> {
    for event in &mut events {
        match event {
            Ok(ModelEvent::MessageStart { usage, .. }) => {
                *usage = Usage {
                    input_tokens: 20,
                    output_tokens: 1,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 4,
                };
            }
            Ok(ModelEvent::MessageDelta { usage, .. }) => usage.output_tokens = 6,
            _ => {}
        }
    }
    events
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
    // 丢弃的标题照样写用量行（Node 在请求结束时就写），之后也没有 `TitleGenerated`。
    until_title_call(&built.store).await;
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

/// 标题请求在请求结束时写一行用量（工作线 F73，usage.md §2.1.1）：单独一批，锚在起标题时的首条输入（标题晚于第二条
/// 输入回来也一样）；没有首字时间；这个 ID 没有消息。
#[tokio::test]
async fn the_title_request_writes_a_usage_row_in_the_prompt_turn() {
    let main = Arc::new(ScriptedModel::new(vec![
        text_response(&["one"]),
        text_response(&["two"]),
    ]));
    let open = Arc::new(Notify::new());
    let served = Arc::new(AtomicUsize::new(0));
    let mut setup = Setup::new(main);
    setup.titles = titles(Arc::new(Gated {
        open: open.clone(),
        served,
    }));
    let built = setup.build().await;
    let mut events = built.session.subscribe(None);
    built
        .session
        .send(submit("c1", "please name this session later"))
        .await;
    until(&mut events, finished).await;
    built
        .session
        .send(submit("c2", "and here is a second message"))
        .await;
    until(&mut events, finished).await;
    open.notify_one();
    until(&mut events, |event| titled(event, "Generated name")).await;

    let session = SessionId::new("sess_1");
    let facts = built.store.facts(&session);
    let [call] = title_calls(&facts).try_into().expect("one title usage row");
    assert_eq!(call.status, CallStatus::Completed);
    assert_eq!(call.stop_reason, Some(StopReason::EndTurn));
    assert_eq!((call.started_at_ms, call.completed_at_ms), (1_000, 1_000));
    assert_eq!(call.first_token_at_ms, None, "no first token, as in Node");
    assert_eq!((call.tool_calls, call.retries), (0, 0));
    assert!(call.error.is_none());
    assert!(
        !facts.iter().any(|fact| matches!(
            fact,
            Fact::MessageAppended { message } if message.id() == &call.message
        )),
        "the row has no assistant message"
    );
    let prompts: Vec<MessageId> = facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::MessageAppended {
                message: message @ Message::User(user),
            } if matches!(user.origin, UserOrigin::Human) => Some(message.id().clone()),
            _ => None,
        })
        .collect();
    assert_eq!(prompts.len(), 2);
    // 最后两批：标题的用量（首条输入的轮次）、生成的标题（当前轮，第二条输入）。
    let turns = built.store.batch_turns(&session);
    let anchor = |index: usize| {
        turns[turns.len() - index]
            .as_ref()
            .map(|turn| turn.prompt.clone())
    };
    assert_eq!(anchor(2), Some(prompts[0].clone()));
    assert_eq!(anchor(1), Some(prompts[1].clone()));
    assert!(matches!(facts.last(), Some(Fact::TitleGenerated { .. })));
}

/// 失败的标题请求也写一行（Node 在失败时同样记 `error`），用量取已经报告的部分。
#[tokio::test]
async fn a_failed_title_request_still_writes_a_usage_row() {
    let main = Arc::new(ScriptedModel::new(vec![text_response(&["Sure."])]));
    let mut events = with_usage(text_response(&["{\"title\": \"Half"]));
    events.truncate(3);
    events.push(Err(ModelError::Overloaded {
        message: "busy".to_owned(),
    }));
    let title_model = Arc::new(ScriptedModel::new(vec![events]));
    let mut setup = Setup::new(main);
    setup.titles = titles(title_model);
    let built = setup.build().await;
    let mut stream = built.session.subscribe(None);
    built
        .session
        .send(submit("c1", "Explain the hook trust digest"))
        .await;
    until(&mut stream, finished).await;
    let call = until_title_call(&built.store).await;
    assert_eq!(call.status, CallStatus::Error);
    assert!(matches!(call.error, Some(ModelError::Overloaded { .. })));
    assert_eq!(call.stop_reason, None);
    assert_eq!(
        call.usage
            .map(|usage| (usage.input_tokens, usage.output_tokens)),
        Some((20, 1))
    );
    let facts = built.store.facts(&SessionId::new("sess_1"));
    assert!(generated(&facts).is_empty());
}
