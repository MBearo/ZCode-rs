use rust_cli_kernel_api::message::ToolResultContent;
use rust_cli_kernel_api::model::ModelError;
use serde_json::json;

use super::*;

fn sse(name: &str, data: Value) -> SseEvent {
    SseEvent {
        event: Some(name.to_owned()),
        data: data.to_string(),
    }
}

fn typed(data: Value) -> SseEvent {
    let name = data["type"].as_str().unwrap().to_owned();
    sse(&name, data)
}

fn start() -> SseEvent {
    typed(json!({"type": "message_start", "message": {
        "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-sonnet-4-5",
        "content": [], "stop_reason": null, "stop_sequence": null,
        "usage": {"input_tokens": 12, "cache_creation_input_tokens": 3,
                  "cache_read_input_tokens": 100, "output_tokens": 1}}}))
}

fn decode(events: Vec<SseEvent>) -> (Vec<ModelEvent>, Option<ModelError>) {
    decode_for(Vendor::Generic, events)
}

/// 按某个厂商的分类器解码（错误块与结束原因先经厂商层，provider-errors.md §2.2）。
fn decode_for(vendor: Vendor, events: Vec<SseEvent>) -> (Vec<ModelEvent>, Option<ModelError>) {
    let mut decoder = AnthropicStream::new(vendor);
    let mut out = VecDeque::new();
    for event in events {
        if let Err(verdict) = decoder.decode(event, &mut out) {
            return (out.into(), Some(verdict.error));
        }
    }
    (out.into(), None)
}

fn usage(input: u64, output: u64, creation: u64, read: u64) -> Usage {
    Usage {
        input_tokens: input,
        output_tokens: output,
        cache_creation_input_tokens: creation,
        cache_read_input_tokens: read,
    }
}

#[test]
fn text_thinking_and_tool_use_map_to_model_events() {
    let (events, error) = decode(vec![
        start(),
        sse("ping", json!({"type": "ping"})),
        typed(json!({"type": "content_block_start", "index": 0,
                     "content_block": {"type": "thinking", "thinking": "", "signature": ""}})),
        typed(json!({"type": "content_block_delta", "index": 0,
                     "delta": {"type": "thinking_delta", "thinking": "Let me "}})),
        typed(json!({"type": "content_block_delta", "index": 0,
                     "delta": {"type": "signature_delta", "signature": "EqQB"}})),
        typed(json!({"type": "content_block_stop", "index": 0})),
        // start 里的文本被忽略：SDK 有时在 start 与 delta 里各给一次。
        typed(json!({"type": "content_block_start", "index": 1,
                     "content_block": {"type": "text", "text": "dup"}})),
        typed(json!({"type": "content_block_delta", "index": 1,
                     "delta": {"type": "text_delta", "text": "dup"}})),
        typed(json!({"type": "content_block_delta", "index": 1,
                     "delta": {"type": "citations_delta", "citation": {}}})),
        typed(json!({"type": "content_block_stop", "index": 1})),
        typed(json!({"type": "content_block_start", "index": 2,
                     "content_block": {"type": "tool_use", "id": "toolu_9", "name": "Read", "input": {}}})),
        typed(json!({"type": "content_block_delta", "index": 2,
                     "delta": {"type": "input_json_delta", "partial_json": "{\"file_"}})),
        typed(json!({"type": "content_block_delta", "index": 2,
                     "delta": {"type": "input_json_delta", "partial_json": "path\":\"/a\"}"}})),
        typed(json!({"type": "content_block_stop", "index": 2})),
        typed(
            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use", "stop_sequence": null},
                     "usage": {"output_tokens": 42}}),
        ),
        typed(json!({"type": "message_stop"})),
    ]);
    assert_eq!(error, None);
    let delta = |index, delta| ModelEvent::BlockDelta { index, delta };
    assert_eq!(
        events,
        vec![
            ModelEvent::MessageStart {
                id: "msg_1".to_owned(),
                model: "claude-sonnet-4-5".to_owned(),
                usage: usage(12, 1, 3, 100),
            },
            ModelEvent::BlockStart {
                index: 0,
                block: ContentBlock::Thinking {
                    thinking: String::new(),
                    signature: Some(String::new()),
                },
            },
            delta(0, BlockDelta::Thinking("Let me ".to_owned())),
            delta(0, BlockDelta::Signature("EqQB".to_owned())),
            ModelEvent::BlockStop { index: 0 },
            ModelEvent::BlockStart {
                index: 1,
                block: ContentBlock::Text {
                    text: String::new()
                },
            },
            delta(1, BlockDelta::Text("dup".to_owned())),
            ModelEvent::BlockStop { index: 1 },
            ModelEvent::BlockStart {
                index: 2,
                block: ContentBlock::ToolUse {
                    id: ToolUseId::new("toolu_9"),
                    name: "Read".to_owned(),
                    input: json!({}),
                },
            },
            delta(2, BlockDelta::InputJson("{\"file_".to_owned())),
            delta(2, BlockDelta::InputJson("path\":\"/a\"}".to_owned())),
            ModelEvent::BlockStop { index: 2 },
            ModelEvent::MessageDelta {
                stop_reason: Some(StopReason::ToolUse),
                usage: usage(12, 42, 3, 100),
            },
            ModelEvent::MessageStop,
        ]
    );
}

#[test]
fn usage_merges_into_the_running_total() {
    let (events, _) = decode(vec![
        start(),
        // input 与缓存 token 为 0 或 null 时保留 message_start 的值；output 只要给出就覆盖。
        typed(
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"},
                     "usage": {"input_tokens": 0, "cache_read_input_tokens": null, "output_tokens": 0}}),
        ),
        typed(
            json!({"type": "message_delta", "delta": {"stop_reason": null},
                     "usage": {"input_tokens": 20, "cache_creation_input_tokens": 5}}),
        ),
        typed(
            json!({"type": "message_delta", "delta": {"stop_reason": "model_context_window_exceeded"},
                     "usage": {}}),
        ),
    ]);
    assert_eq!(
        events[1..],
        [
            ModelEvent::MessageDelta {
                stop_reason: Some(StopReason::EndTurn),
                usage: usage(12, 0, 3, 100),
            },
            ModelEvent::MessageDelta {
                stop_reason: None,
                usage: usage(20, 0, 5, 100),
            },
            // 修改（F113，provider-errors.md D7 暂定）：以前不认识这个值，是 `Unknown`。
            ModelEvent::MessageDelta {
                stop_reason: Some(StopReason::ModelContextWindowExceeded),
                usage: usage(20, 0, 5, 100),
            },
        ]
    );
}

/// 修 bug（F113，D20）：不带事件名、正文是错误形状的块以前被忽略，流读完时只剩 `stream ended before message_stop`。
#[test]
fn unnamed_error_frames_end_the_stream() {
    let (events, error) = decode(vec![
        start(),
        SseEvent {
            event: None,
            data:
                json!({"type": "error", "error": {"type": "overloaded_error", "message": "busy"}})
                    .to_string(),
        },
    ]);
    assert_eq!(events.len(), 1);
    assert_eq!(
        error,
        Some(ModelError::Overloaded {
            message: "busy".to_owned()
        })
    );
    // 不带事件名的普通块照旧忽略。
    let (_, none) = decode(vec![
        start(),
        SseEvent {
            event: None,
            data: json!({"type": "keepalive"}).to_string(),
        },
    ]);
    assert_eq!(none, None);
}

/// 智谱的端点：业务码在流里（带不带事件名）按码表；`sensitive` 是内容拦截；`network_error` 是流内的网络失败。
#[test]
fn vendor_codes_and_finish_reasons_on_the_zhipu_endpoint() {
    let (_, limited) = decode_for(
        zhipu(),
        vec![
            start(),
            SseEvent {
                event: None,
                data: json!({"error": {"code": "1302", "message": "并发数过高"}}).to_string(),
            },
        ],
    );
    assert!(
        matches!(limited, Some(ModelError::RateLimited { .. })),
        "{limited:?}"
    );
    let delta = |reason: &str| {
        typed(json!({"type": "message_delta", "delta": {"stop_reason": reason}, "usage": {}}))
    };
    let (events, error) = decode_for(zhipu(), vec![start(), delta("sensitive")]);
    assert_eq!(error, None);
    assert!(matches!(
        events[1],
        ModelEvent::MessageDelta {
            stop_reason: Some(StopReason::Refusal),
            ..
        }
    ));
    let (events, error) = decode_for(zhipu(), vec![start(), delta("network_error")]);
    assert_eq!(events.len(), 1);
    assert!(
        matches!(error, Some(ModelError::Network { .. })),
        "{error:?}"
    );
    // 别的端点不认这两个值（照旧 `Unknown`）。
    let (events, _) = decode(vec![start(), delta("sensitive")]);
    assert!(matches!(
        events[1],
        ModelEvent::MessageDelta {
            stop_reason: Some(StopReason::Unknown),
            ..
        }
    ));
}

#[test]
fn error_events_are_classified() {
    let (events, error) = decode(vec![
        start(),
        sse(
            "error",
            json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
        ),
    ]);
    assert_eq!(events.len(), 1);
    assert_eq!(
        error,
        Some(ModelError::Overloaded {
            message: "Overloaded".to_owned()
        })
    );
}

#[test]
fn redacted_thinking_and_unsupported_blocks() {
    let (events, error) = decode(vec![
        start(),
        typed(json!({"type": "content_block_start", "index": 0,
                     "content_block": {"type": "redacted_thinking", "data": "opaque"}})),
        typed(json!({"type": "content_block_stop", "index": 0})),
        // 服务端工具的块（tools-more.md §5）：开始事件带内容，增量不输出。
        typed(json!({"type": "content_block_start", "index": 1,
                     "content_block": {"type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search", "input": {}}})),
        typed(json!({"type": "content_block_delta", "index": 1,
                     "delta": {"type": "input_json_delta", "partial_json": "{}"}})),
        typed(json!({"type": "content_block_stop", "index": 1})),
        typed(json!({"type": "content_block_start", "index": 2,
                     "content_block": {"type": "web_search_tool_result", "tool_use_id": "srvtoolu_1",
                                       "content": [{"type": "web_search_result", "title": "T", "url": "https://example.com"}]}})),
        typed(json!({"type": "content_block_stop", "index": 2})),
        // 内核没有形状的块：整个块被跳过。
        typed(json!({"type": "content_block_start", "index": 3,
                     "content_block": {"type": "mcp_tool_use", "id": "x"}})),
        typed(json!({"type": "content_block_stop", "index": 3})),
    ]);
    assert_eq!(error, None);
    assert_eq!(
        events[1..],
        [
            ModelEvent::BlockStart {
                index: 0,
                block: ContentBlock::RedactedThinking {
                    data: "opaque".to_owned()
                },
            },
            ModelEvent::BlockStop { index: 0 },
            ModelEvent::BlockStart {
                index: 1,
                block: ContentBlock::ServerToolUse {
                    id: "srvtoolu_1".to_owned(),
                    name: "web_search".to_owned(),
                    input: json!({}),
                },
            },
            ModelEvent::BlockStop { index: 1 },
            ModelEvent::BlockStart {
                index: 2,
                block: ContentBlock::WebSearchToolResult {
                    tool_use_id: "srvtoolu_1".to_owned(),
                    content: json!([{"type": "web_search_result", "title": "T", "url": "https://example.com"}]),
                },
            },
            ModelEvent::BlockStop { index: 2 },
        ]
    );
}

#[test]
fn events_are_dispatched_by_their_sse_name() {
    let unnamed = SseEvent {
        event: None,
        data: json!({"type": "message_start", "message": {}}).to_string(),
    };
    // 与 SDK 相同：没有 `event:` 名或名字未知的事件被忽略；`message` 名按 data 的 type 处理。
    let (events, error) = decode(vec![
        unnamed,
        sse("future_event", json!({"type": "future_event"})),
        sse(
            "message",
            json!({"type": "message_start", "message": {"id": "m", "model": "x"}}),
        ),
        sse("message_stop", json!({"type": "message_stop"})),
        sse(
            "message_delta",
            json!({"type": "message_delta", "delta": {}, "usage": {}}),
        ),
    ]);
    assert_eq!(error, None);
    assert_eq!(
        events,
        vec![
            ModelEvent::MessageStart {
                id: "m".to_owned(),
                model: "x".to_owned(),
                usage: Usage::default(),
            },
            ModelEvent::MessageStop,
        ]
    );
}

#[test]
fn protocol_violations_are_errors() {
    let block_start = |index, kind: &str| {
        typed(
            json!({"type": "content_block_start", "index": index, "content_block": {"type": kind}}),
        )
    };
    let delta = |kind: &str, field: &str| {
        typed(
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": kind, field: "x"}}),
        )
    };
    let cases = vec![
        vec![block_start(0, "text")],
        vec![start(), delta("text_delta", "text")],
        vec![
            start(),
            block_start(0, "text"),
            delta("input_json_delta", "partial_json"),
        ],
        vec![
            start(),
            block_start(0, "thinking"),
            delta("text_delta", "text"),
        ],
        vec![start(), block_start(0, "text"), block_start(0, "text")],
        vec![
            start(),
            typed(json!({"type": "content_block_stop", "index": 3})),
        ],
        vec![start(), typed(json!({"type": "content_block_stop"}))],
        vec![start(), block_start(0, "tool_use")],
        vec![
            sse("message_start", Value::Null),
            SseEvent {
                event: Some("content_block_delta".to_owned()),
                data: "{not json".to_owned(),
            },
        ],
    ];
    for (case, events) in cases.into_iter().enumerate() {
        let (_, error) = decode(events);
        assert!(
            matches!(error, Some(ModelError::Protocol { .. })),
            "case {case}: {error:?}"
        );
    }
}

/// 工作线 F90（model.md §4.1）：端点在 assistant 一侧用非标准的 `tool_result` 块返回服务端调用的结果（GLM）。
/// `tool_use_id` 是这次响应里已开始的 `server_tool_use` 时解成那次调用的结果：字符串与块数组都按 tool_result 的
/// 内容解，解不了的项写成 JSON 文本，增量不输出。对不上的照旧跳过。修 bug 前所有 `tool_result` 块都被跳过，
/// 跳过的序号在组装时留下空槽（query.md §1.9）。
#[test]
fn a_bare_tool_result_for_a_server_call_is_that_call_result() {
    let (events, error) = decode(vec![
        start(),
        typed(json!({"type": "content_block_start", "index": 1,
                     "content_block": {"type": "server_tool_use", "id": "srvtoolu_g", "name": "web_search",
                                       "input": {"query": "q"}}})),
        typed(json!({"type": "content_block_stop", "index": 1})),
        // 乱序：index 3 先开始。
        typed(json!({"type": "content_block_start", "index": 3,
                     "content_block": {"type": "tool_result", "tool_use_id": "srvtoolu_g", "is_error": true,
                                       "content": "plain"}})),
        typed(json!({"type": "content_block_stop", "index": 3})),
        typed(json!({"type": "content_block_start", "index": 2,
                     "content_block": {"type": "tool_result", "tool_use_id": "srvtoolu_g",
                                       "content": [{"type": "text", "text": "found"},
                                                   {"type": "tool_reference", "tool_name": "WebFetch"},
                                                   {"type": "web_search_result", "url": "https://example.com"}]}})),
        typed(json!({"type": "content_block_delta", "index": 2,
                     "delta": {"type": "text_delta", "text": "ignored"}})),
        typed(json!({"type": "content_block_stop", "index": 2})),
        // 引用的不是这次响应里的服务端调用：跳过，增量与结束也不输出。
        typed(json!({"type": "content_block_start", "index": 4,
                     "content_block": {"type": "tool_result", "tool_use_id": "toolu_other", "content": "x"}})),
        typed(json!({"type": "content_block_stop", "index": 4})),
    ]);
    assert_eq!(error, None);
    let result = |tool_use_id: &str, content: Vec<ToolResultContent>, is_error: bool| {
        ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new(tool_use_id),
            content,
            is_error,
        }
    };
    let text = |text: &str| ToolResultContent::Text {
        text: text.to_owned(),
    };
    assert_eq!(
        events[1..],
        [
            ModelEvent::BlockStart {
                index: 1,
                block: ContentBlock::ServerToolUse {
                    id: "srvtoolu_g".to_owned(),
                    name: "web_search".to_owned(),
                    input: json!({"query": "q"}),
                },
            },
            ModelEvent::BlockStop { index: 1 },
            ModelEvent::BlockStart {
                index: 3,
                block: result("srvtoolu_g", vec![text("plain")], true),
            },
            ModelEvent::BlockStop { index: 3 },
            ModelEvent::BlockStart {
                index: 2,
                block: result(
                    "srvtoolu_g",
                    vec![
                        text("found"),
                        ToolResultContent::ToolReference {
                            tool_name: "WebFetch".to_owned(),
                        },
                        text(r#"{"type":"web_search_result","url":"https://example.com"}"#),
                    ],
                    false,
                ),
            },
            ModelEvent::BlockStop { index: 2 },
        ]
    );
}

/// 智谱的端点按方案表选出的厂商（测试不直接写厂商名，同产品代码只经 `profiles::vendor`）。
fn zhipu() -> Vendor {
    crate::profiles::vendor("https://open.bigmodel.cn/api/anthropic")
}
