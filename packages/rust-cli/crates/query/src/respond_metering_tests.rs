//! 模型请求与工具调用的计时（docs/specs/rust-cli/usage.md §1、§2）：计时点、状态、与消息同批提交。
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::message::{Message, StopReason};
use rust_cli_kernel_api::metering::{CallStatus, Metered, ModelCall, ToolCall};
use rust_cli_kernel_api::model::{
    ModelError, ModelEvent, ModelNotice, ModelPort, ModelRequest, ModelStreamItem, QuerySource,
};
use rust_cli_kernel_api::testing::{
    FakeTool, RecordingSink, ScriptedModel, selection, text_response, tool_use_response,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::run::tests::{parts, request, scope};

/// 每读一次前进 10 ms 的时钟：计时点的先后可以逐个核对。
#[derive(Default)]
struct TickClock(AtomicU64);

impl Clock for TickClock {
    fn now_ms(&self) -> u64 {
        self.0.fetch_add(10, Ordering::SeqCst) + 10
    }

    fn time_zone(&self) -> String {
        "UTC".to_owned()
    }
}

async fn run(
    model: Arc<dyn ModelPort>,
    tools: Vec<Arc<dyn rust_cli_kernel_api::tool::Tool>>,
) -> (
    Result<
        rust_cli_kernel_api::orchestration::QueryStop,
        rust_cli_kernel_api::orchestration::QueryError,
    >,
    Arc<RecordingSink>,
) {
    let mut parts = parts(model, tools);
    parts.clock = Arc::new(TickClock::default());
    let request = request("hi");
    let (scope, sink) = scope(parts, &request, Abort::new());
    (crate::run::run(scope, request).await, sink)
}

fn models(sink: &RecordingSink) -> Vec<(Option<String>, ModelCall)> {
    sink.metered()
        .into_iter()
        .filter_map(|(message, metered)| match metered {
            Metered::Model(call) => Some((message.map(|id| id.as_str().to_owned()), call)),
            Metered::Tool(_) => None,
        })
        .collect()
}

fn tools(sink: &RecordingSink) -> Vec<(Option<String>, ToolCall)> {
    sink.metered()
        .into_iter()
        .filter_map(|(message, metered)| match metered {
            Metered::Tool(call) => Some((message.map(|id| id.as_str().to_owned()), call)),
            Metered::Model(_) => None,
        })
        .collect()
}

#[tokio::test]
async fn a_text_reply_is_timed_and_committed_with_its_assistant_message() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["", "hel", "lo"])]));
    let (stop, sink) = run(model, Vec::new()).await;
    stop.unwrap();
    let calls = models(&sink);
    assert_eq!(calls.len(), 1);
    let (message, call) = &calls[0];
    let assistant = sink
        .messages()
        .into_iter()
        .find_map(|m| match m {
            Message::Assistant(a) => Some(a),
            _ => None,
        })
        .unwrap();
    assert_eq!(message.as_deref(), Some(assistant.id.as_str()));
    assert_eq!(call.message, assistant.id);
    assert_eq!(call.source, QuerySource::MainThread);
    assert_eq!(call.selection, selection());
    // 请求前、第一个非空的文本增量（空串不算）、流结束各读一次时钟。
    assert_eq!(
        (
            call.started_at_ms,
            call.first_token_at_ms,
            call.completed_at_ms
        ),
        (10, Some(20), 30)
    );
    assert_eq!(call.status, CallStatus::Completed);
    assert_eq!(call.stop_reason, Some(StopReason::EndTurn));
    assert_eq!((call.tool_calls, call.retries), (0, 0));
    assert!(call.error.is_none());
}

#[tokio::test]
async fn a_tool_round_commits_the_tool_call_with_its_result() {
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Echo", &json!({})),
        text_response(&["done"]),
    ]));
    let tool: Arc<dyn rust_cli_kernel_api::tool::Tool> =
        Arc::new(FakeTool::new("Echo", true, "héllo"));
    let (stop, sink) = run(model, vec![tool]).await;
    stop.unwrap();
    let calls = models(&sink);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].1.tool_calls, 1);
    assert_eq!(calls[0].1.stop_reason, Some(StopReason::ToolUse));
    let tool_calls = tools(&sink);
    assert_eq!(tool_calls.len(), 1);
    let (message, call) = &tool_calls[0];
    let result = sink
        .messages()
        .into_iter()
        .find(|m| matches!(m, Message::User(u) if matches!(u.origin, rust_cli_kernel_api::message::UserOrigin::ToolResult { .. })))
        .unwrap();
    assert_eq!(message.as_deref(), Some(result.id().as_str()));
    assert_eq!(call.tool_use.as_str(), "toolu_1");
    assert_eq!(call.name, "Echo");
    assert_eq!(call.status, CallStatus::Completed);
    // `héllo` 是 6 个 UTF-8 字节。
    assert_eq!(call.output_bytes, 6);
    assert!(call.started_at_ms <= call.completed_at_ms);
    assert!(calls[0].1.started_at_ms < call.started_at_ms);
}

#[tokio::test]
async fn an_unknown_tool_is_timed_as_an_error_without_running() {
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Missing", &json!({})),
        text_response(&["done"]),
    ]));
    let (stop, sink) = run(model, Vec::new()).await;
    stop.unwrap();
    let tool_calls = tools(&sink);
    assert_eq!(tool_calls.len(), 1);
    let call = &tool_calls[0].1;
    assert_eq!(call.status, CallStatus::Error);
    assert_eq!(call.started_at_ms, call.completed_at_ms);
}

#[tokio::test]
async fn a_failed_request_without_content_is_committed_alone() {
    let model = Arc::new(ScriptedModel::new(vec![vec![Err(
        ModelError::InvalidRequest {
            message: "bad".to_owned(),
        },
    )]]));
    let (stop, sink) = run(model, Vec::new()).await;
    assert!(stop.is_err());
    let calls = models(&sink);
    assert_eq!(calls.len(), 1);
    let (message, call) = &calls[0];
    assert_eq!(message, &None);
    assert_eq!(call.status, CallStatus::Error);
    assert_eq!(
        call.error,
        Some(ModelError::InvalidRequest {
            message: "bad".to_owned()
        })
    );
    assert_eq!(call.first_token_at_ms, None);
}

/// 先发两次重试通知、再给文本响应的模型（适配器内部的重试算在同一次调用里）。
struct RetryingModel;

impl ModelPort for RetryingModel {
    fn stream(
        &self,
        _request: ModelRequest,
        _cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
        stream::iter(text_response(&["ok"])).boxed()
    }

    fn stream_with_notices(
        &self,
        _request: ModelRequest,
        _cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ModelStreamItem, ModelError>> {
        let retry = || {
            Ok(ModelStreamItem::Notice(ModelNotice::Retrying {
                attempt: 1,
                max_retries: 10,
                delay_ms: 0,
                error: ModelError::Overloaded {
                    message: "busy".to_owned(),
                },
            }))
        };
        let events = text_response(&["ok"])
            .into_iter()
            .map(|event| event.map(ModelStreamItem::Event));
        stream::iter([retry(), retry()].into_iter().chain(events)).boxed()
    }
}

#[tokio::test]
async fn adapter_retries_are_counted_in_the_same_call() {
    let (stop, sink) = run(Arc::new(RetryingModel), Vec::new()).await;
    stop.unwrap();
    let calls = models(&sink);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1.retries, 2);
    assert_eq!(calls[0].1.status, CallStatus::Completed);
}
