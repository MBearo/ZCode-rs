//! 模型轨迹的记录（docs/specs/rust-cli/model-io.md §4、§6）：每次 HTTP 发送一条记录，请求同轨迹比对基线的形状，
//! 回复是原始 SSE 事件与由它们拼出的 Node 形状的回复字段（F93）；跳过的块、重试、错误正文、取消、提前结束、看门狗与
//! 丢弃的流各有结局。

#![allow(
    clippy::unwrap_used,
    reason = "辅助函数不在 #[test] 函数里，clippy 的测试放行不覆盖它；失败即测试失败"
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_kernel_api::ids::{RunId, SessionId};
use rust_cli_kernel_api::model::{ModelError, ModelPort, ModelRequest, QuerySource};
use rust_cli_model::{
    ByteIdle, ModelIoRecord, ModelIoSink, ProviderModel, RequestHeaders, StreamIdle,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::common::{
    API_KEY, block_start, block_stop, collect, delta, event, fast_retry, message_end,
    message_start, model_at, request, started_text, text_response,
};
use super::fixture::{Fixture, Reply};

/// 把记录转成 JSON 收起来（入口的写者在自己的线程上做同样的转换）。
#[derive(Default)]
struct Collected(Mutex<Vec<Value>>);

impl ModelIoSink for Collected {
    fn record(&self, record: ModelIoRecord) {
        self.0
            .lock()
            .unwrap()
            .push(Value::Object(record.into_json()));
    }
}

impl Collected {
    fn take(&self) -> Vec<Value> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

fn headers() -> RequestHeaders {
    RequestHeaders {
        source: Vec::new(),
        trace_id: "trace-io".to_owned(),
        request_id: Arc::new(|| "req-io".to_owned()),
    }
}

async fn recorded(server: &Fixture, retries: u32) -> (ProviderModel, Arc<Collected>) {
    let sink = Arc::new(Collected::default());
    let model = model_at(server.url(), fast_retry(retries))
        .await
        .with_request_headers(headers())
        .with_model_io(sink.clone());
    (model, sink)
}

fn main_request() -> ModelRequest {
    let mut request = request("hello there", QuerySource::MainThread);
    request.options.session_id = Some(SessionId::new("sess_io"));
    request.options.run = Some(RunId::new("run_io"));
    request
}

fn stream_reply(body: String) -> Reply {
    Reply::new(200)
        .header("content-type", "text/event-stream")
        .header("request-id", "req_server_1")
        .send(body)
}

/// 文本块之后是一个解码器不认识的块。
fn reply_with_unknown_block() -> String {
    let mut stream = message_start();
    stream.push_str(&block_start(0, json!({"type": "text", "text": ""})));
    stream.push_str(&delta(0, json!({"type": "text_delta", "text": "hi"})));
    stream.push_str(&block_stop(0));
    stream.push_str(&block_start(
        1,
        json!({"type": "tool_result", "tool_use_id": "srv_1", "content": "odd"}),
    ));
    stream.push_str(&block_stop(1));
    stream.push_str(&message_end("end_turn", 9));
    stream
}

#[tokio::test]
async fn a_reply_is_recorded_with_the_request_as_sent_and_the_raw_events() {
    let server = Fixture::start(vec![stream_reply(reply_with_unknown_block())]).await;
    let (model, sink) = recorded(&server, 0).await;
    let events = collect(&model, main_request()).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    let records = sink.take();
    assert_eq!(records.len(), 1, "{records:#?}");
    let record = &records[0];
    let text = record.to_string();
    assert!(!text.contains(API_KEY), "the API key leaked: {text}");
    assert_eq!(record["type"], "model_io");
    assert_eq!(record["requestId"], "req-io");
    assert_eq!(record["attempt"], 1);
    assert_eq!(record["startedAt"], "2020-09-13T12:26:40.000Z");
    assert_eq!(record["sessionId"], "sess_io");
    assert_eq!(record["turnId"], "run_io");
    assert_eq!(record["traceId"], "trace-io");
    assert_eq!(record["querySource"], "main_turn");
    assert_eq!(
        record["model"],
        json!({"modelId": "claude-sonnet-4-5", "providerId": "anthropic",
               "protocol": "anthropic-messages", "profile": "anthropic-compatible"})
    );
    let request = &record["request"];
    assert_eq!(request["route"], "messages");
    assert_eq!(request["class"], "main");
    assert_eq!(request["method"], "POST");
    assert_eq!(request["path"], "/v1/messages?beta=true");
    assert_eq!(request["headers"]["x-api-key"], "[redacted]");
    assert_eq!(request["headers"]["content-type"], "application/json");
    assert_eq!(request["headers"]["x-request-id"], "req-io");
    // 请求体就是发出的那份 JSON。
    let sent: Value = serde_json::from_str(&server.requests()[0].body).unwrap();
    assert_eq!(request["body"], sent);
    let response = &record["response"];
    assert_eq!(response["status"], 200);
    assert_eq!(response["requestId"], "req_server_1");
    assert_eq!(response["headers"]["content-type"], "text/event-stream");
    let chunks = response["data"]["chunks"].as_array().unwrap();
    assert_eq!(response["data"]["stream"], true);
    assert_eq!(chunks.len(), 8, "{chunks:#?}");
    assert_eq!(chunks[0]["type"], "message_start");
    assert_eq!(chunks[4]["content_block"]["type"], "tool_result");
    assert_eq!(chunks[7]["type"], "message_stop");
    assert_eq!(response["responseId"], "msg_01");
    assert_eq!(response["modelId"], "claude-sonnet-4-5");
    // 工作线 F93（model-io.md §4.2）：App 读的回复字段照 Node 的形状——停止原因是 AI SDK 的统一值，用量是 Node 的键名，
    // 文字由事件拼出；F91 时是 `end_turn` 与 Anthropic 的键名。原值在 `chunks` 里。
    assert_eq!(response["finishReason"], "stop");
    assert_eq!(
        response["usage"],
        json!({"inputTokens": 25, "outputTokens": 9, "totalTokens": 34,
               "cacheReadTokens": 0, "cacheWriteTokens": 0})
    );
    assert_eq!(response["text"], "hi");
    assert_eq!(response["toolCalls"], json!([]));
    assert!(response.get("reasoningText").is_none());
    assert_eq!(
        record["decoder"],
        json!({"skippedBlocks": [{"index": 1, "type": "tool_result"}]})
    );
    assert_eq!(record["outcome"], "completed");
    assert!(record.get("error").is_none() && record.get("retry").is_none());
}

/// 推理、两段文字、一个本地调用（参数分两段）与一个服务端调用（参数在原始事件的增量里，解码器不输出），停在 tool_use。
fn reply_with_calls() -> String {
    let mut stream = event(json!({"type": "message_start", "message": {
        "id": "msg_calls", "type": "message", "role": "assistant", "model": "claude-sonnet-4-5",
        "content": [], "stop_reason": null, "stop_sequence": null,
        "usage": {"input_tokens": 20, "cache_creation_input_tokens": 3,
                  "cache_read_input_tokens": 5, "output_tokens": 1}}}));
    stream.push_str(&block_start(0, json!({"type": "thinking", "thinking": ""})));
    stream.push_str(&delta(
        0,
        json!({"type": "thinking_delta", "thinking": "Need the file."}),
    ));
    stream.push_str(&delta(
        0,
        json!({"type": "signature_delta", "signature": "sig"}),
    ));
    stream.push_str(&block_stop(0));
    stream.push_str(&block_start(1, json!({"type": "text", "text": ""})));
    stream.push_str(&delta(1, json!({"type": "text_delta", "text": "Let me "})));
    stream.push_str(&block_stop(1));
    stream.push_str(&block_start(
        2,
        json!({"type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search", "input": {}}),
    ));
    stream.push_str(&delta(
        2,
        json!({"type": "input_json_delta", "partial_json": "{\"query\":"}),
    ));
    stream.push_str(&delta(
        2,
        json!({"type": "input_json_delta", "partial_json": "\"rust\"}"}),
    ));
    stream.push_str(&block_stop(2));
    stream.push_str(&block_start(3, json!({"type": "text", "text": ""})));
    stream.push_str(&delta(3, json!({"type": "text_delta", "text": "check."})));
    stream.push_str(&block_stop(3));
    stream.push_str(&block_start(
        4,
        json!({"type": "tool_use", "id": "toolu_1", "name": "Read", "input": {}}),
    ));
    stream.push_str(&delta(
        4,
        json!({"type": "input_json_delta", "partial_json": "{\"path\":"}),
    ));
    stream.push_str(&delta(
        4,
        json!({"type": "input_json_delta", "partial_json": "\"a.txt\"}"}),
    ));
    stream.push_str(&block_stop(4));
    stream.push_str(&message_end("tool_use", 7));
    stream
}

/// 工作线 F93（model-io.md §4.2，Node `runner-debug.ts`）：Node 形状的回复字段由事件拼出——文字直接连接、推理用空行连接、
/// 结束了的调用按块序号（服务端调用带 `providerExecuted`）、停止原因与用量换成 Node 的写法；原始事件照旧完整。流中途断开
/// 时没结束的调用不写。F91 只记原始事件，App 的面板读不到回复文字与工具调用。
#[tokio::test]
async fn node_shaped_reply_fields_are_built_from_the_events() {
    let mut short = message_start();
    short.push_str(&block_start(0, json!({"type": "text", "text": ""})));
    short.push_str(&delta(0, json!({"type": "text_delta", "text": "partial"})));
    short.push_str(&block_stop(0));
    short.push_str(&block_start(
        1,
        json!({"type": "tool_use", "id": "toolu_cut", "name": "Read", "input": {}}),
    ));
    short.push_str(&delta(
        1,
        json!({"type": "input_json_delta", "partial_json": "{\"pa"}),
    ));
    let server = Fixture::start(vec![stream_reply(reply_with_calls()), stream_reply(short)]).await;
    let (model, sink) = recorded(&server, 0).await;
    let events = collect(&model, main_request()).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    let events = collect(&model, main_request()).await;
    assert!(events.last().unwrap().is_err(), "{events:?}");
    let records = sink.take();
    assert_eq!(records.len(), 2, "{records:#?}");
    let response = &records[0]["response"];
    assert_eq!(response["text"], "Let me check.");
    assert_eq!(response["reasoningText"], "Need the file.");
    assert_eq!(
        response["toolCalls"],
        json!([
            {"id": "srvtoolu_1", "name": "web_search", "input": {"query": "rust"}, "providerExecuted": true},
            {"id": "toolu_1", "name": "Read", "input": {"path": "a.txt"}}
        ])
    );
    assert_eq!(response["finishReason"], "tool-calls");
    assert_eq!(
        response["usage"],
        json!({"inputTokens": 28, "outputTokens": 7, "totalTokens": 35,
               "cacheReadTokens": 5, "cacheWriteTokens": 3})
    );
    assert_eq!(response["data"]["chunks"].as_array().unwrap().len(), 21);
    // 断开的那次：已结束的文字照写，没结束的调用不写，没有停止原因。
    let cut = &records[1]["response"];
    assert_eq!(records[1]["outcome"], "failed");
    assert_eq!(cut["text"], "partial");
    assert_eq!(cut["toolCalls"], json!([]));
    assert!(cut.get("finishReason").is_none(), "{cut:#}");
}

#[tokio::test]
async fn each_attempt_is_a_record_and_a_failed_one_keeps_the_error_body() {
    let server = Fixture::start(vec![
        Reply::new(500)
            .header("content-type", "application/json")
            .send(r#"{"type":"error","error":{"type":"api_error","message":"boom"}}"#),
        stream_reply(text_response(&["ok"])),
    ])
    .await;
    let (model, sink) = recorded(&server, 1).await;
    let events = collect(&model, main_request()).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    let records = sink.take();
    assert_eq!(records.len(), 2, "{records:#?}");
    let first = &records[0];
    assert_eq!(first["attempt"], 1);
    assert_eq!(first["outcome"], "failed");
    assert_eq!(first["response"]["status"], 500);
    assert_eq!(first["response"]["data"]["error"]["message"], "boom");
    assert_eq!(first["error"]["name"], "server");
    assert!(first["retry"]["delayMs"].is_u64(), "{first:#}");
    // 请求体每次都是完整的一份（压缩是写者的事）。
    assert_eq!(first["request"]["body"], records[1]["request"]["body"]);
    assert_eq!(records[1]["attempt"], 2);
    assert_eq!(records[1]["outcome"], "completed");
    assert!(records[1].get("decoder").is_none());
}

#[tokio::test]
async fn cancelling_dropping_and_a_short_stream_have_their_outcomes() {
    let server = Fixture::start(vec![
        stream_reply(started_text("par")).hang(),
        stream_reply(started_text("par")).hang(),
        stream_reply(started_text("par")),
    ])
    .await;
    let (model, sink) = recorded(&server, 0).await;
    // 取消：收到第一个事件后取消。
    let cancel = CancellationToken::new();
    let mut stream = model.stream(main_request(), cancel.clone());
    assert!(stream.next().await.unwrap().is_ok());
    cancel.cancel();
    assert_eq!(stream.next().await, Some(Err(ModelError::Cancelled)));
    drop(stream);
    // 丢弃：收到第一个事件后不再读。
    let mut stream = model.stream(main_request(), CancellationToken::new());
    assert!(stream.next().await.unwrap().is_ok());
    drop(stream);
    // 提前结束：连接关了还没有 message_stop。
    let events = collect(&model, main_request()).await;
    assert!(events.last().unwrap().is_err(), "{events:?}");
    let records = sink.take();
    let outcomes: Vec<&Value> = records.iter().map(|record| &record["outcome"]).collect();
    assert_eq!(
        outcomes,
        ["cancelled", "abandoned", "failed"],
        "{records:#?}"
    );
    assert_eq!(records[0]["error"]["name"], "cancelled");
    assert!(records[1].get("error").is_none());
    assert_eq!(records[2]["error"]["name"], "protocol");
    assert_eq!(
        records[2]["error"]["message"],
        "protocol error: stream ended before message_stop"
    );
    for record in &records {
        assert_eq!(
            record["response"]["data"]["chunks"][0]["type"],
            "message_start"
        );
    }
}

#[tokio::test]
async fn the_idle_watchdog_is_recorded_with_its_silent_resend() {
    let server = Fixture::start(vec![
        stream_reply(String::new()).hang(),
        stream_reply(text_response(&["ok"])),
    ])
    .await;
    let (model, sink) = recorded(&server, 3).await;
    let model = model.with_stream_idle(StreamIdle {
        events: Some(Duration::from_millis(10_000)),
        bytes: Some(ByteIdle::Fixed(Duration::from_millis(150))),
        stall_notice: Duration::from_millis(50),
    });
    let events = collect(&model, main_request()).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    let records = sink.take();
    assert_eq!(records.len(), 2, "{records:#?}");
    assert_eq!(records[0]["outcome"], "idle_timeout");
    assert_eq!(records[0]["error"]["name"], "stream_idle");
    assert_eq!(records[0]["retry"], json!({"delayMs": 0}));
    assert_eq!(records[1]["outcome"], "completed");
}

#[tokio::test]
async fn nothing_is_recorded_without_a_sink() {
    // 没有注入记录方时（测试与没有写者的组装）请求照常，不产生记录。
    let server = Fixture::start(vec![stream_reply(
        event(json!({"type": "ping"})) + &text_response(&["ok"]),
    )])
    .await;
    let model = model_at(server.url(), fast_retry(0)).await;
    let events = collect(&model, main_request()).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
}
