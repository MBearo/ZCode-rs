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

/// 每个 tool_use 块的名字与拼好的参数（InputJson 按块号拼接）；块结束之后再来的参数直接判失败。
fn calls(events: &[ModelEvent]) -> Vec<(String, String)> {
    let mut calls: Vec<(usize, String, String, bool)> = Vec::new();
    for event in events {
        match event {
            ModelEvent::BlockStart {
                index,
                block: ContentBlock::ToolUse { name, .. },
            } => calls.push((*index, name.clone(), String::new(), false)),
            ModelEvent::BlockDelta {
                index,
                delta: BlockDelta::InputJson(more),
            } => {
                let call = calls.iter_mut().find(|call| call.0 == *index).unwrap();
                assert!(!call.3, "arguments after block {index} stopped");
                call.2.push_str(more);
            }
            ModelEvent::BlockStop { index } => {
                if let Some(call) = calls.iter_mut().find(|call| call.0 == *index) {
                    call.3 = true;
                }
            }
            _ => {}
        }
    }
    calls
        .into_iter()
        .map(|(_, name, arguments, stopped)| {
            assert!(stopped, "{name} was not stopped");
            (name, arguments)
        })
        .collect()
}

/// 一个函数调用的三个事件：`added`（`arguments` 为空）、`function_call_arguments.done`、`output_item.done`，
/// 中间没有 `function_call_arguments.delta`（有的供应商只这样发）。
fn call_without_deltas(index: u64, name: &str, arguments: &str) -> Vec<Value> {
    let item = |arguments: &str| {
        json!({"type": "function_call", "id": format!("fc_{index}"), "call_id": format!("call_{index}"),
            "name": name, "arguments": arguments})
    };
    let mut done = item(arguments);
    done["status"] = json!("completed");
    vec![
        json!({"type": "response.output_item.added", "output_index": index, "item": item("")}),
        json!({"type": "response.function_call_arguments.done", "output_index": index,
            "item_id": format!("fc_{index}"), "arguments": arguments}),
        json!({"type": "response.output_item.done", "output_index": index, "item": done}),
    ]
}

fn turn(items: Vec<Vec<Value>>) -> Vec<Value> {
    let mut events = vec![created()];
    events.extend(items.into_iter().flatten());
    events.push(completed("completed", None));
    events
}

#[test]
fn arguments_only_in_the_done_events_reach_the_call() {
    let arguments = r#"{"skill": "architecture-governance"}"#;
    let events = decode(turn(vec![call_without_deltas(0, "Skill", arguments)])).unwrap();
    assert_eq!(
        events[1..4],
        [
            ModelEvent::BlockStart {
                index: 0,
                block: ContentBlock::ToolUse {
                    id: ToolUseId::new("call_0"),
                    name: "Skill".to_owned(),
                    input: Value::Object(Map::new()),
                },
            },
            ModelEvent::BlockDelta {
                index: 0,
                delta: BlockDelta::InputJson(arguments.to_owned()),
            },
            ModelEvent::BlockStop { index: 0 },
        ]
    );
    assert!(matches!(
        events[4],
        ModelEvent::MessageDelta {
            stop_reason: Some(StopReason::ToolUse),
            ..
        }
    ));
}

#[test]
fn parallel_calls_without_deltas_each_keep_their_arguments() {
    let expected = [
        (
            "Bash",
            r#"{"command": "git status", "description": "Show status"}"#,
        ),
        ("Bash", r#"{"command": "git log --oneline -5"}"#),
        ("Skill", r#"{"skill": "architecture-governance"}"#),
        ("Bash", r#"{"command": "ls docs"}"#),
    ];
    let items = (0..)
        .zip(expected)
        .map(|(index, (name, arguments))| call_without_deltas(index, name, arguments))
        .collect();
    let events = decode(turn(items)).unwrap();
    let expected: Vec<_> = expected
        .iter()
        .map(|(name, arguments)| ((*name).to_owned(), (*arguments).to_owned()))
        .collect();
    assert_eq!(calls(&events), expected);
}

#[test]
fn streamed_arguments_are_written_once() {
    let arguments = r#"{"command": "ls"}"#;
    let events = decode(turn(vec![vec![
        json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "function_call",
            "id": "fc_0", "call_id": "call_0", "name": "Bash", "arguments": ""}}),
        json!({"type": "response.function_call_arguments.delta", "output_index": 0, "item_id": "fc_0", "delta": r#"{"command""#}),
        json!({"type": "response.function_call_arguments.delta", "output_index": 0, "item_id": "fc_0", "delta": r#": "ls"}"#}),
        json!({"type": "response.function_call_arguments.done", "output_index": 0, "item_id": "fc_0", "arguments": arguments}),
        json!({"type": "response.output_item.done", "output_index": 0, "item": {"type": "function_call",
            "id": "fc_0", "call_id": "call_0", "name": "Bash", "arguments": arguments, "status": "completed"}}),
    ]]))
    .unwrap();
    assert_eq!(calls(&events), [("Bash".to_owned(), arguments.to_owned())]);
}

#[test]
fn chinese_arguments_without_deltas_are_kept_verbatim() {
    let arguments = r#"{"file_path": "/Users/dev/笔记.md", "content": "中文参数：第一行\n第二行"}"#;
    let events = decode(turn(vec![call_without_deltas(0, "Write", arguments)])).unwrap();
    assert_eq!(calls(&events), [("Write".to_owned(), arguments.to_owned())]);
}

#[test]
fn the_complete_arguments_win_over_different_deltas() {
    let arguments = r#"{"command": "ls -la"}"#;
    let events = decode(turn(vec![vec![
        json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "function_call",
            "id": "fc_0", "call_id": "call_0", "name": "Bash", "arguments": ""}}),
        json!({"type": "response.function_call_arguments.delta", "output_index": 0, "item_id": "fc_0", "delta": r#"{"command": "l"#}),
        json!({"type": "response.output_item.done", "output_index": 0, "item": {"type": "function_call",
            "id": "fc_0", "call_id": "call_0", "name": "Bash", "arguments": arguments, "status": "completed"}}),
    ]]))
    .unwrap();
    assert_eq!(calls(&events), [("Bash".to_owned(), arguments.to_owned())]);
}

#[test]
fn deltas_are_used_when_the_done_event_has_no_arguments() {
    let events = decode(turn(vec![
        vec![
            json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "function_call",
                "id": "fc_0", "call_id": "call_0", "name": "Read", "arguments": r#"{"file_path""#}}),
            json!({"type": "response.function_call_arguments.delta", "output_index": 0, "delta": r#": "a.txt"}"#}),
            json!({"type": "response.output_item.done", "output_index": 0, "item": {"type": "function_call", "id": "fc_0"}}),
        ],
        // 到 `response.completed` 还没有 done 的调用：同样用攒下的参数。
        vec![
            json!({"type": "response.output_item.added", "output_index": 1, "item": {"type": "function_call",
                "id": "fc_1", "call_id": "call_1", "name": "Glob", "arguments": ""}}),
            json!({"type": "response.function_call_arguments.delta", "output_index": 1, "delta": r#"{"pattern": "*.md"}"#}),
        ],
    ]))
    .unwrap();
    assert_eq!(
        calls(&events),
        [
            ("Read".to_owned(), r#"{"file_path": "a.txt"}"#.to_owned()),
            ("Glob".to_owned(), r#"{"pattern": "*.md"}"#.to_owned()),
        ]
    );
}

#[test]
fn a_call_done_twice_is_a_protocol_error() {
    let mut events = call_without_deltas(0, "Read", "{}");
    events.push(events[2].clone());
    assert!(matches!(
        decode(turn(vec![events])),
        Err(ModelError::Protocol { .. })
    ));
}
