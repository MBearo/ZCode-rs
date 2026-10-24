//! 并行工具在流还开着时完成：结果按完成先后交出（docs/specs/rust-cli/query.md §1.6）。
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::message::{ContentBlock, Message};
use rust_cli_kernel_api::model::{ModelError, ModelEvent, ModelPort, ModelRequest};
use rust_cli_kernel_api::orchestration::{QueryEvent, QueryStop};
use rust_cli_kernel_api::testing::{FakeTool, RecordingSink, text_response};
use rust_cli_kernel_api::tool::Tool;
use serde_json::json;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::executor::tests::tool_uses;
use crate::run::tests::{parts, request, scope};

type Events = Vec<Result<ModelEvent, ModelError>>;

/// 第一次响应发到最后一个块结束就停住，等 `release` 才发 `message_delta` / `message_stop`；之后按序回复。
struct HeldModel {
    first: Mutex<Option<(Events, Events)>>,
    rest: Mutex<Vec<Events>>,
    release: Arc<Notify>,
}

impl ModelPort for HeldModel {
    fn stream(
        &self,
        _request: ModelRequest,
        _cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
        if let Some((head, tail)) = self.first.lock().unwrap().take() {
            let release = self.release.clone();
            let tail = stream::once(async move {
                release.notified().await;
                stream::iter(tail)
            })
            .flatten();
            return stream::iter(head).chain(tail).boxed();
        }
        let next = self.rest.lock().unwrap().remove(0);
        stream::iter(next).boxed()
    }
}

/// 等到 sink 里出现这个调用的 `ToolFinished`。
async fn finished(sink: &RecordingSink, id: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let done = sink.events().iter().any(|event| {
                matches!(event, QueryEvent::ToolFinished { tool_use, .. } if tool_use.as_str() == id)
            });
            if done {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

/// 提交的消息：assistant 记 `A`，工具结果记调用 ID。
fn committed(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .map(|message| match message {
            Message::Assistant(_) => "A".to_owned(),
            Message::User(user) => match user.content.first() {
                Some(ContentBlock::ToolResult { tool_use_id, .. }) => {
                    tool_use_id.as_str().to_owned()
                }
                _ => "U".to_owned(),
            },
            other => format!("{other:?}"),
        })
        .collect()
}

/// 两个并发安全的调用都在流结束之前完成、后加入的先完成：结果按完成先后交出，assistant 在它们之前。
/// 以前流结束之后才开始交出，两个都已完成，按加入顺序给出 [t1, t2]。
#[tokio::test]
async fn results_finished_while_the_stream_is_open_keep_their_finishing_order() {
    let gate = Arc::new(Notify::new());
    let slow = Arc::new(FakeTool::new("ReadA", true, "a").gated(gate.clone()));
    let fast = Arc::new(FakeTool::new("ReadB", true, "b"));
    let mut head = tool_uses(&[("t1", "ReadA", json!({})), ("t2", "ReadB", json!({}))]);
    let tail = head.split_off(head.len() - 2);
    let release = Arc::new(Notify::new());
    let model = Arc::new(HeldModel {
        first: Mutex::new(Some((head, tail))),
        rest: Mutex::new(vec![text_response(&["ok"])]),
        release: release.clone(),
    });
    let tools: Vec<Arc<dyn Tool>> = vec![slow.clone(), fast];
    let request = request("go");
    let (scope, sink) = scope(parts(model, tools), &request, Abort::new());
    let running = tokio::spawn(crate::run::run(scope, request));
    finished(&sink, "t2").await;
    gate.notify_one();
    finished(&sink, "t1").await;
    // 两个都完成了，流还开着：结果还没有提交（assistant 还没有）。
    assert!(sink.messages().is_empty(), "{:?}", sink.messages());
    release.notify_one();
    assert_eq!(running.await.unwrap().unwrap(), QueryStop::Completed);
    assert_eq!(committed(&sink.messages()), ["A", "t2", "t1", "A"]);
}

/// 流还开着时找不到的工具（加入时就有结果）先交出，排在还在执行的并发安全调用之前。
#[tokio::test]
async fn an_unknown_tool_added_while_the_stream_is_open_is_handed_out_first() {
    let gate = Arc::new(Notify::new());
    let slow = Arc::new(FakeTool::new("ReadA", true, "a").gated(gate.clone()));
    let mut head = tool_uses(&[("t1", "ReadA", json!({})), ("t2", "Nope", json!({}))]);
    let tail = head.split_off(head.len() - 2);
    let release = Arc::new(Notify::new());
    let model = Arc::new(HeldModel {
        first: Mutex::new(Some((head, tail))),
        rest: Mutex::new(vec![text_response(&["ok"])]),
        release: release.clone(),
    });
    let request = request("go");
    let (scope, sink) = scope(parts(model, vec![slow.clone()]), &request, Abort::new());
    let running = tokio::spawn(crate::run::run(scope, request));
    tokio::time::timeout(Duration::from_secs(5), slow.started.notified())
        .await
        .unwrap();
    gate.notify_one();
    finished(&sink, "t1").await;
    release.notify_one();
    assert_eq!(running.await.unwrap().unwrap(), QueryStop::Completed);
    assert_eq!(committed(&sink.messages()), ["A", "t2", "t1", "A"]);
}
