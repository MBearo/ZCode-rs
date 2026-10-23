//! Stop / SubagentStop hooks 在主循环里的效果（docs/specs/rust-cli/hooks.md §7、query.md §1）。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::hooks::{HookEvent, HookEventName, HookOutcome, HookRunner};
use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::{ContentBlock, Message, UserMessage, UserOrigin};
use rust_cli_kernel_api::orchestration::QueryStop;
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::run::tests::{parts, request, scope};

/// 按顺序返回给定的结果，并记下收到的事件。
#[derive(Default)]
struct ScriptedHooks {
    outcomes: Mutex<VecDeque<HookOutcome>>,
    events: Mutex<Vec<HookEvent>>,
}

#[async_trait]
impl HookRunner for ScriptedHooks {
    async fn run(&self, event: HookEvent, _cancel: CancellationToken) -> HookOutcome {
        self.events.lock().unwrap().push(event);
        self.outcomes
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_default()
    }
}

fn feedback(text: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new("msg_hook"),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        origin: UserOrigin::Meta {
            source: "hook_blocking_error".to_owned(),
        },
    })
}

fn blocking(text: &str) -> HookOutcome {
    HookOutcome {
        blocking_error: Some(text.to_owned()),
        messages: vec![feedback(text)],
        ..HookOutcome::default()
    }
}

async fn run(
    responses: usize,
    outcomes: Vec<HookOutcome>,
    max_turns: Option<u32>,
) -> (QueryStop, Vec<Message>, Vec<HookEvent>, usize) {
    let model = Arc::new(ScriptedModel::new(
        (0..responses)
            .map(|index| text_response(&[&format!("reply {index}")]))
            .collect(),
    ));
    let hooks = Arc::new(ScriptedHooks {
        outcomes: Mutex::new(outcomes.into()),
        events: Mutex::default(),
    });
    let mut parts = parts(model.clone(), Vec::new());
    parts.hooks = hooks.clone();
    let mut request = request("go");
    request.max_turns = max_turns;
    let (scope, sink) = scope(parts, &request, Abort::new());
    let stop = crate::run::run(scope, request).await.unwrap();
    let events = hooks.events.lock().unwrap().clone();
    (stop, sink.messages(), events, model.requests().len())
}

#[tokio::test]
async fn the_stop_hook_sees_the_last_reply_and_lets_the_turn_end() {
    let (stop, _messages, events, requests) = run(1, Vec::new(), None).await;
    assert_eq!(stop, QueryStop::Completed);
    assert_eq!(requests, 1);
    let [event] = events.as_slice() else {
        panic!("expected one Stop event: {events:?}");
    };
    assert_eq!(event.name, HookEventName::Stop);
    assert_eq!(
        event.payload,
        json!({
            "stop_hook_active": false,
            "last_assistant_message": "reply 0",
            "background_tasks": [],
            "session_crons": [],
        })
    );
    assert!(event.context.mode.is_some());
}

#[tokio::test]
async fn a_blocking_stop_hook_continues_the_turn_with_its_feedback() {
    let (stop, messages, events, requests) = run(
        2,
        vec![blocking("Stop hook feedback:\nrun the tests")],
        None,
    )
    .await;
    assert_eq!(stop, QueryStop::Completed);
    assert_eq!(requests, 2);
    assert!(messages.contains(&feedback("Stop hook feedback:\nrun the tests")));
    // 第二次 Stop 时 stop_hook_active 为真。
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].payload["stop_hook_active"], json!(true));
}

#[tokio::test]
async fn a_prevented_turn_ends_without_another_request() {
    let prevented = HookOutcome {
        prevent_continuation: true,
        stop_reason: Some("done for today".to_owned()),
        messages: vec![feedback("done for today")],
        ..HookOutcome::default()
    };
    let (stop, messages, _events, requests) = run(2, vec![prevented], None).await;
    assert_eq!(stop, QueryStop::Completed);
    assert_eq!(requests, 1);
    assert!(messages.contains(&feedback("done for today")));
}

#[tokio::test]
async fn blocking_stops_at_max_turns_and_at_the_block_cap() {
    // maxTurns = 1：阻塞后 turnCount 变 2，超过上限。
    let (stop, messages, _events, requests) =
        run(2, vec![blocking("Stop hook feedback:\nagain")], Some(1)).await;
    assert_eq!(stop, QueryStop::MaxTurns);
    assert_eq!(requests, 1);
    assert!(messages.iter().any(|message| matches!(
        message,
        Message::Attachment(attachment) if attachment.source == "max_turns_reached"
    )));
    // 连续阻塞 9 次：第 9 次超过上限 8，按 completed 结束，不再请求。
    let outcomes = (0..9)
        .map(|_| blocking("Stop hook feedback:\nagain"))
        .collect();
    let (stop, _messages, events, requests) = run(10, outcomes, None).await;
    assert_eq!(stop, QueryStop::Completed);
    assert_eq!(requests, 9);
    assert_eq!(events.len(), 9);
}
