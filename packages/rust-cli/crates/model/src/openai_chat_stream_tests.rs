//! OpenAI Chat Completions 流解码（model.md §10.3）。

use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::message::ContentBlock;
use rust_cli_kernel_api::model::BlockDelta;
use serde_json::{Map, json};

use super::*;

fn data(value: Value) -> SseEvent {
    SseEvent {
        event: None,
        data: value.to_string(),
    }
}

fn done() -> SseEvent {
    SseEvent {
        event: None,
        data: "[DONE]".to_owned(),
    }
}

fn decode(events: Vec<SseEvent>) -> Result<Vec<ModelEvent>, ModelError> {
    let mut stream = ChatStream::default();
    let mut out = VecDeque::new();
    for event in events {
        stream.decode(event, &mut out)?;
    }
    assert!(stream.finished(), "stream did not finish");
    Ok(out.into_iter().collect())
}

fn chunk(delta: Value) -> SseEvent {
    data(json!({"id": "chatcmpl-1", "model": "glm-5", "choices": [{"index": 0, "delta": delta}]}))
}

#[test]
fn reasoning_text_and_tool_calls_become_blocks() {
    let events = decode(vec![
        chunk(json!({"role": "assistant", "reasoning_content": "think"})),
        chunk(json!({"reasoning_content": "ing"})),
        chunk(json!({"content": "Let me look."})),
        chunk(json!({"tool_calls": [{"index": 0, "id": "call_a", "type": "function",
            "function": {"name": "Read", "arguments": "{\"file"}}]})),
        chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": "_path\":\"a\"}"}}]})),
        chunk(json!({"tool_calls": [{"index": 1, "id": "call_b", "function": {"name": "Glob", "arguments": ""}}]})),
        data(json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]})),
        data(json!({"choices": [], "usage": {"prompt_tokens": 120, "completion_tokens": 30,
            "prompt_tokens_details": {"cached_tokens": 100}}})),
        done(),
    ])
    .unwrap();
    let tool = |id: &str, name: &str| ContentBlock::ToolUse {
        id: ToolUseId::new(id),
        name: name.to_owned(),
        input: Value::Object(Map::new()),
    };
    assert_eq!(
        events,
        vec![
            ModelEvent::MessageStart {
                id: "chatcmpl-1".to_owned(),
                model: "glm-5".to_owned(),
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
                delta: BlockDelta::Thinking("think".to_owned()),
            },
            ModelEvent::BlockDelta {
                index: 0,
                delta: BlockDelta::Thinking("ing".to_owned()),
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
                delta: BlockDelta::Text("Let me look.".to_owned()),
            },
            ModelEvent::BlockStop { index: 1 },
            ModelEvent::BlockStart {
                index: 2,
                block: tool("call_a", "Read"),
            },
            ModelEvent::BlockDelta {
                index: 2,
                delta: BlockDelta::InputJson("{\"file".to_owned()),
            },
            ModelEvent::BlockDelta {
                index: 2,
                delta: BlockDelta::InputJson("_path\":\"a\"}".to_owned()),
            },
            ModelEvent::BlockStart {
                index: 3,
                block: tool("call_b", "Glob"),
            },
            ModelEvent::BlockStop { index: 2 },
            ModelEvent::BlockStop { index: 3 },
            ModelEvent::MessageDelta {
                stop_reason: Some(StopReason::ToolUse),
                usage: Usage {
                    input_tokens: 20,
                    output_tokens: 30,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 100,
                },
            },
            ModelEvent::MessageStop,
        ]
    );
}

#[test]
fn finish_reasons_map_to_stop_reasons() {
    for (reason, expected) in [
        ("stop", StopReason::EndTurn),
        ("length", StopReason::MaxTokens),
        ("content_filter", StopReason::Refusal),
        ("something_new", StopReason::Unknown),
    ] {
        let events = decode(vec![
            data(json!({"choices": [{"delta": {"content": "x"}, "finish_reason": reason}]})),
            done(),
        ])
        .unwrap();
        let stop = events.iter().find_map(|event| match event {
            ModelEvent::MessageDelta { stop_reason, .. } => *stop_reason,
            _ => None,
        });
        assert_eq!(stop, Some(expected), "{reason}");
    }
}

#[test]
fn missing_call_ids_get_a_placeholder_and_bad_streams_fail() {
    let events = decode(vec![
        chunk(
            json!({"tool_calls": [{"index": 3, "function": {"name": "Read", "arguments": "{}"}}]}),
        ),
        done(),
    ])
    .unwrap();
    assert!(events.iter().any(|event| matches!(event,
        ModelEvent::BlockStart { block: ContentBlock::ToolUse { id, .. }, .. } if id.as_str() == "call_3")));
    // 有调用却没有 finish_reason：按工具调用结束。
    assert!(events.iter().any(|event| matches!(
        event,
        ModelEvent::MessageDelta {
            stop_reason: Some(StopReason::ToolUse),
            ..
        }
    )));

    let mut stream = ChatStream::default();
    let mut out = VecDeque::new();
    let error = stream.decode(
        data(json!({"error": {"message": "quota", "code": "insufficient_quota"}})),
        &mut out,
    );
    assert!(matches!(error, Err(ModelError::RateLimited { .. })));
    let invalid = ChatStream::default().decode(
        SseEvent {
            event: None,
            data: "{not json".to_owned(),
        },
        &mut out,
    );
    assert!(matches!(invalid, Err(ModelError::Protocol { .. })));
    let unknown = decode(vec![
        chunk(json!({"content": "x"})),
        chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": "{}"}}]})),
    ]);
    assert!(matches!(unknown, Err(ModelError::Protocol { .. })));
    assert!(matches!(
        decode(vec![done()]),
        Err(ModelError::Protocol { .. })
    ));
}

/// 每个 tool_use 块的 (id, 名字, 拼好的参数)。
fn calls(events: &[ModelEvent]) -> Vec<(String, String, String)> {
    let mut calls: Vec<(usize, String, String, String)> = Vec::new();
    for event in events {
        match event {
            ModelEvent::BlockStart {
                index,
                block: ContentBlock::ToolUse { id, name, .. },
            } => calls.push((*index, id.as_str().to_owned(), name.clone(), String::new())),
            ModelEvent::BlockDelta {
                index,
                delta: BlockDelta::InputJson(more),
            } => calls
                .iter_mut()
                .find(|call| call.0 == *index)
                .unwrap()
                .3
                .push_str(more),
            _ => {}
        }
    }
    calls
        .into_iter()
        .map(|(_, id, name, arguments)| (id, name, arguments))
        .collect()
}

#[test]
fn arguments_given_whole_in_the_last_chunk_are_kept() {
    let events = decode(vec![
        chunk(json!({"tool_calls": [{"index": 0, "id": "call_a", "type": "function",
            "function": {"name": "Write", "arguments": ""}}]})),
        data(json!({"choices": [{"index": 0, "finish_reason": "tool_calls", "delta": {"tool_calls": [{"index": 0,
            "function": {"arguments": "{\"file_path\":\"/Users/dev/笔记.md\",\"content\":\"中文\"}"}}]}}]})),
        done(),
    ])
    .unwrap();
    assert_eq!(
        calls(&events),
        [(
            "call_a".to_owned(),
            "Write".to_owned(),
            "{\"file_path\":\"/Users/dev/笔记.md\",\"content\":\"中文\"}".to_owned()
        )]
    );
}

#[test]
fn calls_without_an_index_are_new_calls() {
    let events = decode(vec![
        chunk(json!({"tool_calls": [{"id": "call_a", "type": "function",
            "function": {"name": "Bash", "arguments": "{\"command\":\"ls\"}"}}]})),
        chunk(json!({"tool_calls": [{"id": "call_b", "type": "function",
            "function": {"name": "Read", "arguments": "{\"file_path\":\"a.txt\"}"}}]})),
        data(json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]})),
        done(),
    ])
    .unwrap();
    assert_eq!(
        calls(&events),
        [
            (
                "call_a".to_owned(),
                "Bash".to_owned(),
                "{\"command\":\"ls\"}".to_owned()
            ),
            (
                "call_b".to_owned(),
                "Read".to_owned(),
                "{\"file_path\":\"a.txt\"}".to_owned()
            ),
        ]
    );
}
