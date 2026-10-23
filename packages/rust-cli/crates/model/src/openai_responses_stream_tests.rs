//! OpenAI Responses 流解码（model.md §10.3）。

use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::message::ContentBlock;
use rust_cli_kernel_api::model::BlockDelta;
use serde_json::{Map, json};

use super::*;

fn event(value: Value) -> SseEvent {
    SseEvent {
        event: value["type"].as_str().map(str::to_owned),
        data: value.to_string(),
    }
}

fn decode(events: Vec<Value>) -> Result<Vec<ModelEvent>, ModelError> {
    let mut stream = ResponsesStream::default();
    let mut out = VecDeque::new();
    for value in events {
        stream.decode(event(value), &mut out)?;
    }
    Ok(out.into_iter().collect())
}

fn created() -> Value {
    json!({"type": "response.created", "response": {"id": "resp_1", "model": "gpt-5.4", "status": "in_progress"}})
}

fn completed(status: &str, reason: Option<&str>) -> Value {
    let kind = if status == "completed" {
        "response.completed"
    } else {
        "response.incomplete"
    };
    json!({"type": kind, "response": {"id": "resp_1", "status": status,
        "incomplete_details": reason.map(|reason| json!({"reason": reason})),
        "usage": {"input_tokens": 50, "output_tokens": 7, "input_tokens_details": {"cached_tokens": 40}}}})
}

#[test]
fn reasoning_text_and_calls_become_blocks() {
    let events = decode(vec![
        created(),
        json!({"type": "response.output_item.added", "output_index": 0, "item": {"id": "rs_1", "type": "reasoning"}}),
        json!({"type": "response.reasoning_summary_text.delta", "output_index": 0, "item_id": "rs_1", "delta": "plan"}),
        json!({"type": "response.output_item.done", "output_index": 0, "item": {"id": "rs_1", "type": "reasoning", "summary": [{"type": "summary_text", "text": "plan"}]}}),
        json!({"type": "response.output_item.added", "output_index": 1, "item": {"id": "msg_1", "type": "message", "role": "assistant"}}),
        json!({"type": "response.output_text.delta", "output_index": 1, "item_id": "msg_1", "delta": "Hi"}),
        json!({"type": "response.output_item.done", "output_index": 1, "item": {"id": "msg_1", "type": "message"}}),
        json!({"type": "response.output_item.added", "output_index": 2, "item": {"id": "fc_1", "type": "function_call", "call_id": "call_1", "name": "Read", "arguments": ""}}),
        json!({"type": "response.function_call_arguments.delta", "output_index": 2, "item_id": "fc_1", "delta": "{}"}),
        json!({"type": "response.output_item.done", "output_index": 2, "item": {"id": "fc_1", "type": "function_call", "arguments": "{}"}}),
        completed("completed", None),
    ])
    .unwrap();
    assert_eq!(
        events,
        vec![
            ModelEvent::MessageStart {
                id: "resp_1".to_owned(),
                model: "gpt-5.4".to_owned(),
                usage: Usage::default(),
            },
            ModelEvent::BlockStart {
                index: 0,
                block: ContentBlock::Thinking {
                    thinking: String::new(),
                    signature: None,
                },
            },
            ModelEvent::BlockDelta {
                index: 0,
                delta: BlockDelta::Thinking("plan".to_owned()),
            },
            ModelEvent::BlockStop { index: 0 },
            ModelEvent::BlockStart {
                index: 1,
                block: ContentBlock::Text {
                    text: String::new(),
                },
            },
            ModelEvent::BlockDelta {
                index: 1,
                delta: BlockDelta::Text("Hi".to_owned()),
            },
            ModelEvent::BlockStop { index: 1 },
            ModelEvent::BlockStart {
                index: 2,
                block: ContentBlock::ToolUse {
                    id: ToolUseId::new("call_1"),
                    name: "Read".to_owned(),
                    input: Value::Object(Map::new()),
                },
            },
            ModelEvent::BlockDelta {
                index: 2,
                delta: BlockDelta::InputJson("{}".to_owned()),
            },
            ModelEvent::BlockStop { index: 2 },
            ModelEvent::MessageDelta {
                stop_reason: Some(StopReason::ToolUse),
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 7,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 40,
                },
            },
            ModelEvent::MessageStop,
        ]
    );
}

#[test]
fn items_without_deltas_are_filled_from_the_done_event() {
    let events = decode(vec![
        created(),
        json!({"type": "response.output_item.added", "output_index": 0, "item": {"id": "msg_1", "type": "message"}}),
        json!({"type": "response.output_item.done", "output_index": 0, "item": {"id": "msg_1", "type": "message",
            "content": [{"type": "output_text", "text": "whole"}, {"type": "refusal", "refusal": " no"}]}}),
        json!({"type": "response.output_item.added", "output_index": 1, "item": {"id": "ws_1", "type": "web_search_call"}}),
        json!({"type": "response.output_item.done", "output_index": 1, "item": {"id": "ws_1", "type": "web_search_call"}}),
        completed("incomplete", Some("max_output_tokens")),
    ])
    .unwrap();
    assert!(events.contains(&ModelEvent::BlockDelta {
        index: 0,
        delta: BlockDelta::Text("whole no".to_owned()),
    }));
    assert!(events.iter().any(|event| matches!(
        event,
        ModelEvent::MessageDelta {
            stop_reason: Some(StopReason::MaxTokens),
            ..
        }
    )));
    assert_eq!(events.last(), Some(&ModelEvent::MessageStop));
}

#[test]
fn failures_and_violations_end_the_stream() {
    let failed = decode(vec![
        created(),
        json!({"type": "response.failed", "response": {"error": {"code": "server_is_overloaded", "message": "busy"}}}),
    ]);
    assert!(matches!(failed, Err(ModelError::Overloaded { .. })));
    let error = decode(vec![
        json!({"type": "error", "code": "rate_limit_exceeded", "message": "slow"}),
    ]);
    assert!(matches!(error, Err(ModelError::RateLimited { .. })));
    let early = decode(vec![
        json!({"type": "response.output_text.delta", "output_index": 0, "delta": "x"}),
    ]);
    assert!(matches!(early, Err(ModelError::Protocol { .. })));
    let unknown = decode(vec![
        created(),
        json!({"type": "response.output_text.delta", "output_index": 4, "delta": "x"}),
    ]);
    assert!(matches!(unknown, Err(ModelError::Protocol { .. })));
    let mismatch = decode(vec![
        created(),
        json!({"type": "response.output_item.added", "output_index": 0, "item": {"id": "rs", "type": "reasoning"}}),
        json!({"type": "response.output_text.delta", "output_index": 0, "delta": "x"}),
    ]);
    assert!(matches!(mismatch, Err(ModelError::Protocol { .. })));
}
