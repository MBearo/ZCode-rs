use std::collections::VecDeque;

use rust_cli_base::secret::RedactedString;
use rust_cli_net::HeaderValue;
use rust_cli_net::sse::SseEvent;
use serde_json::json;

use super::*;
use crate::anthropic_stream::AnthropicStream;
use crate::model_io_tape::Capture;

#[test]
fn secret_and_credential_like_headers_are_redacted() {
    let headers = vec![
        (
            "X-Api-Key".to_owned(),
            HeaderValue::Plain("sk-plain".into()),
        ),
        (
            "x-provider".to_owned(),
            HeaderValue::Secret(RedactedString::new("configured-value")),
        ),
        ("x-session-token".to_owned(), HeaderValue::Plain("t".into())),
        (
            "Authorization".to_owned(),
            HeaderValue::Plain("Bearer x".into()),
        ),
        (
            "x-note".to_owned(),
            HeaderValue::Plain("Bearer abcdefghijklmnopqrstuvwxyz".into()),
        ),
        (
            "accept".to_owned(),
            HeaderValue::Plain("application/json".into()),
        ),
        (
            "accept".to_owned(),
            HeaderValue::Plain("text/event-stream".into()),
        ),
    ];
    assert_eq!(
        request_headers(&headers),
        [
            ("x-api-key", "[redacted]"),
            ("x-provider", "[redacted]"),
            ("x-session-token", "[redacted]"),
            ("authorization", "[redacted]"),
            ("x-note", "Bearer [REDACTED]"),
            ("accept", "text/event-stream"),
        ]
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
    );
    assert_eq!(
        response_headers(vec![
            ("set-cookie".to_owned(), "a=b".to_owned()),
            ("request-id".to_owned(), "req_1".to_owned()),
        ]),
        [
            ("set-cookie".to_owned(), "[redacted]".to_owned()),
            ("request-id".to_owned(), "req_1".to_owned())
        ]
    );
}

#[test]
fn the_path_keeps_the_query_and_hides_secrets() {
    assert_eq!(
        path_of("https://api.example.com/v1/messages?beta=true"),
        "/v1/messages?beta=true"
    );
    assert_eq!(path_of("http://127.0.0.1:9"), "/");
    assert_eq!(
        path_of("https://gw.example.com/v1/chat?key=abcdefghijkl"),
        "/v1/chat?key=[REDACTED]"
    );
}

#[test]
fn the_body_hides_the_user_id_and_image_and_video_data() {
    let body = json!({
        "model": "m",
        "metadata": {"user_id": "{\"device_id\":\"d\"}"},
        "messages": [{"role": "user", "content": [
            {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AAAA"}},
            {"type": "document", "source": {"type": "base64", "media_type": "application/pdf", "data": "BBBB"}},
            {"type": "image_url", "image_url": {"url": "data:IMAGE/jpeg;base64,CCCC"}},
            {"type": "text", "text": "data:text/plain,hi"},
        ]}],
    });
    let redacted = redact_body(body);
    assert_eq!(redacted["metadata"]["user_id"], "[REDACTED]");
    let content = &redacted["messages"][0]["content"];
    assert_eq!(
        content[0]["source"]["data"],
        "[data omitted from model-io: image/png, 4 chars]"
    );
    assert_eq!(content[1]["source"]["data"], "BBBB");
    assert_eq!(
        content[2]["image_url"]["url"],
        "[url omitted from model-io: IMAGE/jpeg, 27 chars]"
    );
    assert_eq!(content[3]["text"], "data:text/plain,hi");
}

fn sse(name: &str, data: &str) -> SseEvent {
    SseEvent {
        event: Some(name.to_owned()),
        data: data.to_owned(),
    }
}

#[test]
fn the_recording_decoder_keeps_raw_events_and_finds_skipped_blocks() {
    let capture = Capture::shared();
    let mut decoder = Capture::wrap(&capture, Box::new(AnthropicStream::default()), None);
    let mut out = VecDeque::new();
    let events = [
        sse(
            "message_start",
            r#"{"type":"message_start","message":{"id":"msg_1","model":"m","usage":{"input_tokens":3}}}"#,
        ),
        sse(
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"s","name":"web_search","input":{}}}"#,
        ),
        sse(
            "content_block_start",
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"mystery"}}"#,
        ),
        sse("ping", "not json"),
        SseEvent {
            event: None,
            data: "[DONE]".to_owned(),
        },
        sse(
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":4}}"#,
        ),
        sse("message_stop", r#"{"type":"message_stop"}"#),
    ];
    for event in events {
        decoder.decode(event, &mut out).unwrap();
    }
    assert!(decoder.finished());
    let taken = capture.lock().unwrap().take();
    assert_eq!(taken.skipped, [(1, "mystery".to_owned())]);
    assert_eq!(taken.chunks.len(), 7);
    assert_eq!(taken.chunks[3], "not json");
    assert_eq!(taken.summary.id.as_deref(), Some("msg_1"));
    assert_eq!(
        taken.summary.usage.map(|usage| usage.output_tokens),
        Some(4)
    );
    // `[DONE]` 与不是 JSON 的行在拼 JSON 时处理：前者不记，后者留原文。
    let response = response_json(ResponsePart {
        status: 200,
        headers: Vec::new(),
        body: ResponseBody::Stream(taken.chunks),
        summary: taken.summary,
    });
    let chunks = response["data"]["chunks"].as_array().unwrap();
    assert_eq!(chunks.len(), 6);
    assert_eq!(chunks[3], "not json");
    // 工作线 F93：停止原因是 AI SDK 的统一值（`end_turn` → `stop`）；服务端调用的块没有结束事件，不写进 `toolCalls`。
    assert_eq!(response["finishReason"], "stop");
    assert_eq!(response["toolCalls"], serde_json::json!([]));
}

/// 工作线 F93（model-io.md §4.2）：停止原因与用量换成 Node 的写法（`mapAnthropicStopReason`、`normalizeUsage`）。
#[test]
fn stop_reasons_and_usage_take_the_node_names() {
    use rust_cli_kernel_api::message::{StopReason, Usage};
    let cases = [
        (StopReason::EndTurn, "stop"),
        (StopReason::StopSequence, "stop"),
        (StopReason::PauseTurn, "stop"),
        (StopReason::ToolUse, "tool-calls"),
        (StopReason::MaxTokens, "length"),
        (StopReason::ModelContextWindowExceeded, "length"),
        (StopReason::Refusal, "content-filter"),
        (StopReason::Unknown, "other"),
    ];
    for (reason, name) in cases {
        assert_eq!(crate::model_io_reply::finish_reason(reason), name);
    }
    let usage = Usage {
        input_tokens: 10,
        output_tokens: 4,
        cache_creation_input_tokens: 2,
        cache_read_input_tokens: 30,
    };
    assert_eq!(
        crate::model_io_reply::usage(usage),
        serde_json::json!({"inputTokens": 42, "outputTokens": 4, "totalTokens": 46,
                           "cacheReadTokens": 30, "cacheWriteTokens": 2})
    );
}
