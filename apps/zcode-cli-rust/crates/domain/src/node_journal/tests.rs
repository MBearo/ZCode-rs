//! The journal's records, read back with the Node-aligned cold readers, give
//! the live model context and rows.
use super::{Op, Write};
use super::{Outcome, Prompt};
use crate::node_history::{Record, hydrate};
use crate::session::Session;
use serde_json::{Map, Value, json};

/// Applies the writes like Node's upserts: messages and parts keep their first
/// position; a promotion saves its message and parts.
pub fn records(writes: &[Write]) -> Vec<Record> {
    let mut messages: Map<String, Value> = Map::new();
    let mut parts: Map<String, Value> = Map::new();
    let save_part = |parts: &mut Map<String, Value>, part: &Value| {
        let message = part["messageID"].as_str().unwrap().to_owned();
        let id = part["id"].as_str().unwrap();
        parts.entry(message).or_insert_with(|| json!({}))[id] = part.clone();
    };
    for write in writes {
        match &write.op {
            Op::Message(info) => {
                messages.insert(info["id"].as_str().unwrap().into(), info.clone());
            }
            Op::Part(part) => save_part(&mut parts, part),
            Op::PromoteInput {
                message,
                parts: list,
                ..
            } => {
                messages.insert(message["id"].as_str().unwrap().into(), message.clone());
                for part in list {
                    save_part(&mut parts, part);
                }
            }
            Op::RemoveMessage(id) => {
                messages.shift_remove(id);
                parts.shift_remove(id);
            }
            _ => {}
        }
    }
    messages
        .into_iter()
        .map(|(id, info)| Record {
            info,
            parts: parts
                .get(&id)
                .and_then(Value::as_object)
                .map(|p| p.values().cloned().collect())
                .unwrap_or_default(),
        })
        .collect()
}

fn session() -> Session {
    let mut s = Session::new(
        "sess_1".into(),
        "/w".into(),
        "p".into(),
        "m".into(),
        "high".into(),
        "e".into(),
        1,
    );
    s.workspace_path = Some("/w".into());
    s
}

fn ids() -> impl FnMut() -> String {
    let mut n = 0;
    move || {
        n += 1;
        format!("00000000-0000-4000-8000-{n:012}")
    }
}

#[test]
fn a_tool_turn_reads_back_as_the_live_context() {
    let mut s = session();
    let mut next = ids();
    s.node_ensure_created(10, "Fix it", "0.0.0");
    s.node_user_prompt(
        10,
        Prompt {
            message: "msg_user".into(),
            part: "part_user".into(),
            turn: "t1",
            text: "Fix it",
            command: Some("cmd_1"),
            queue_id: Some("queue_cmd_1"),
            metadata: None,
            tools: &["Read".into()],
        },
    );
    s.node_step_started(20, ("msg_a1".into(), "part_s1".into()), "p", "m");
    s.node_model_status(
        &json!({"type": "model_request_completed", "finishReason": "tool-calls",
        "usage": {"inputTokens": 100, "outputTokens": 5, "totalTokens": 105}}),
    );
    let first = json!({"role": "assistant", "content": "Let me read.", "reasoning_content": "Look",
        "tool_calls": [{"id": "call_1", "type": "function",
            "function": {"name": "Read", "arguments": "{\"file_path\":\"a.ts\"}"}}],
        "_zcode_origin": {"provider": "p", "model": "m"}});
    s.node_model_done(30, Some(&first), &mut next);
    s.node_tool_started(31, "call_1");
    s.node_tool_done(32, "call_1", (&json!("1\tx"), None, false), &mut next);
    s.node_step_started(40, ("msg_a2".into(), "part_s2".into()), "p", "m");
    s.node_model_status(
        &json!({"type": "model_request_completed", "finishReason": "stop",
        "usage": {"inputTokens": 120, "outputTokens": 2}}),
    );
    let last = json!({"role": "assistant", "content": "Done.", "_zcode_origin": {"provider": "p", "model": "m"}});
    s.node_model_done(50, Some(&last), &mut next);
    s.node_finished(51, Outcome::Success, &mut next);

    let writes = s.node.take();
    assert!(
        matches!(&writes[0].op, Op::CreateSession(c) if c["title"] == "Fix it" && c["titleSource"] == "first_input")
    );
    assert!(matches!(writes.last().map(|w| &w.op),
        Some(Op::StableBoundary { boundary, start, rounds: 2, turn }) if boundary == "msg_a2" && start == "msg_user" && turn == "turn_t1"));
    let records = records(&writes);
    let canonical: Vec<Value> = hydrate(&records, &|_| None)
        .entries
        .iter()
        .map(|e| e.canonical())
        .collect();
    assert_eq!(
        canonical,
        vec![
            json!({"role": "user", "content": "Fix it"}),
            first,
            json!({"role": "tool", "tool_call_id": "call_1", "content": "1\tx", "_zcode_tool_failed": false}),
            last,
        ]
    );
    let assistant = &records[1].info;
    assert_eq!(assistant["finish"], "tool-calls");
    assert_eq!(
        assistant["tokens"],
        json!({"total": 105, "input": 100, "output": 5, "reasoning": 0, "cache": {"read": 0, "write": 0}})
    );
    assert_eq!(assistant["anchor"], json!({"turnId": "turn_t1"}));
    let types: Vec<&str> = records[1]
        .parts
        .iter()
        .map(|p| p["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        ["step-start", "reasoning", "text", "tool", "step-finish"]
    );
    assert_eq!(records[1].parts[3]["state"]["status"], "completed");
}

#[test]
fn cancellation_keeps_the_streamed_output_and_marks_the_turn_cancelled() {
    let mut s = session();
    let mut next = ids();
    s.node_ensure_created(10, "go", "0.0.0");
    s.node_user_prompt(
        10,
        Prompt {
            message: "msg_user".into(),
            part: "part_user".into(),
            turn: "t1",
            text: "go",
            command: None,
            queue_id: None,
            metadata: None,
            tools: &[],
        },
    );
    s.node_step_started(20, ("msg_a1".into(), "part_s1".into()), "p", "m");
    s.node_finished(
        30,
        Outcome::Cancelled {
            text: "partial",
            reasoning: "",
        },
        &mut next,
    );
    let records = records(&s.node.take());
    let assistant = &records[1];
    assert_eq!(assistant.info["error"]["data"]["turnResult"], "cancelled");
    assert_eq!(assistant.info["time"]["completed"], 30);
    assert!(assistant.info.get("finish").is_none());
    let canonical: Vec<Value> = hydrate(&records, &|_| None)
        .entries
        .iter()
        .map(|e| e.canonical())
        .collect();
    assert_eq!(
        canonical[1],
        json!({"role": "assistant", "content": "partial", "_zcode_origin": {"provider": "p", "model": "m"}})
    );
}
