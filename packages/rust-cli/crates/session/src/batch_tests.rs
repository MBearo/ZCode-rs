//! 回合结束后的排队输入成批（修复线 L-A 的 A5，docs/specs/rust-cli/query.md §4.3）：同 mode 的普通文字连着成批进
//! 一个回合；斜杠命令、`@` 提及与 bash 单独。

use std::sync::Arc;

use rust_cli_kernel_api::message::{ContentBlock, Message, UserOrigin};
use rust_cli_kernel_api::model::{ApiContent, ApiRole};
use rust_cli_kernel_api::session_api::{Command, Delivery, KernelEvent, SessionHandle};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};

use crate::test_support::{GatedModel, Setup, envelope, finished, until};

fn queued(
    id: &str,
    text: &str,
    delivery: Delivery,
) -> rust_cli_kernel_api::session_api::CommandEnvelope {
    envelope(
        id,
        Command::Submit {
            text: text.to_owned(),
            attachments: Vec::new(),
            delivery,
        },
    )
}

/// 一个请求最后一条 user 消息里的文字块。
fn last_user_texts(request: &rust_cli_kernel_api::model::ModelRequest) -> Vec<String> {
    let user = request
        .messages
        .iter()
        .rev()
        .find(|message| message.role == ApiRole::User)
        .expect("a user message");
    user.content
        .iter()
        .filter_map(|ApiContent { block, .. }| match block {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn human_texts(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user) if user.origin == UserOrigin::Human => {
                match user.content.as_slice() {
                    [ContentBlock::Text { text }] => Some(text.clone()),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect()
}

/// 第一个请求等放行，之后按剧本回（记下全部请求）。
struct FirstGated {
    gated: GatedModel,
    first: std::sync::atomic::AtomicBool,
    rest: ScriptedModel,
}

impl rust_cli_kernel_api::model::ModelPort for FirstGated {
    fn stream(
        &self,
        request: rust_cli_kernel_api::model::ModelRequest,
        cancel: tokio_util::sync::CancellationToken,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rust_cli_kernel_api::model::ModelEvent, rust_cli_kernel_api::model::ModelError>,
    > {
        if self.first.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return self.gated.stream(request, cancel);
        }
        self.rest.stream(request, cancel)
    }
}

#[tokio::test]
async fn consecutive_plain_inputs_run_as_one_turn() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let model = Arc::new(FirstGated {
        gated: GatedModel::new(gate.clone()),
        first: std::sync::atomic::AtomicBool::new(true),
        rest: ScriptedModel::new(vec![text_response(&["both"]), text_response(&["slash"])]),
    });
    let built = Setup::new(model.clone()).build().await;
    let mut events = built.session.subscribe(Some(0));
    built
        .session
        .send(queued("c1", "hello", Delivery::Queue))
        .await;
    until(&mut events, |event| {
        matches!(event, KernelEvent::TurnStarted { .. })
    })
    .await;
    // 运行中：一条插话（next）、一条排队（later）、一个斜杠命令（不成批，单独一个回合）。
    built
        .session
        .send(queued("c2", "first queued", Delivery::Steer))
        .await;
    built
        .session
        .send(queued("c3", "second queued", Delivery::Queue))
        .await;
    built
        .session
        .send(queued("c4", "/unknown-thing", Delivery::Queue))
        .await;
    gate.notify_one();
    until(&mut events, finished).await;
    until(&mut events, finished).await;
    until(&mut events, finished).await;
    let requests = model.rest.requests();
    // 第一个回合的请求不在剧本模型里；之后两个回合：成批的两条，与斜杠命令。
    assert_eq!(requests.len(), 2, "{requests:#?}");
    assert_eq!(
        last_user_texts(&requests[0]),
        ["first queued\n".to_owned(), "second queued".to_owned()]
    );
    assert_eq!(last_user_texts(&requests[1]), ["/unknown-thing".to_owned()]);
    let view = built.session.view().await.unwrap();
    assert_eq!(
        human_texts(&view.messages),
        ["hello", "first queued", "second queued", "/unknown-thing"]
    );
}

#[tokio::test]
async fn a_mention_after_the_first_input_starts_its_own_turn() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let model = Arc::new(FirstGated {
        gated: GatedModel::new(gate.clone()),
        first: std::sync::atomic::AtomicBool::new(true),
        rest: ScriptedModel::new(vec![text_response(&["one"]), text_response(&["two"])]),
    });
    let built = Setup::new(model.clone()).build().await;
    let mut events = built.session.subscribe(Some(0));
    built
        .session
        .send(queued("c1", "hello", Delivery::Queue))
        .await;
    until(&mut events, |event| {
        matches!(event, KernelEvent::TurnStarted { .. })
    })
    .await;
    built
        .session
        .send(queued("c2", "plain one", Delivery::Queue))
        .await;
    built
        .session
        .send(queued("c3", "look at @a.txt", Delivery::Queue))
        .await;
    gate.notify_one();
    until(&mut events, finished).await;
    until(&mut events, finished).await;
    until(&mut events, finished).await;
    let requests = model.rest.requests();
    assert_eq!(requests.len(), 2, "{requests:#?}");
    assert_eq!(last_user_texts(&requests[0]), ["plain one".to_owned()]);
}
