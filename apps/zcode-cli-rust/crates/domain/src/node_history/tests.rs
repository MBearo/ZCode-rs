use super::*;
use serde_json::{Value, json};

fn record(id: &str, role: &str, parts: Vec<Value>) -> Record {
    Record {
        info: json!({"id": id, "role": role}),
        parts,
    }
}

fn ids(records: &[std::borrow::Cow<Record>]) -> Vec<String> {
    records.iter().map(|r| r.id().to_owned()).collect()
}

#[test]
fn legacy_rewinds_without_a_cut_follow_the_created_message() {
    let messages: Vec<Record> = ["u1", "a1", "u2", "a2", "n", "u3"]
        .iter()
        .map(|id| record(id, "user", vec![]))
        .collect();
    let branch = Branch::from_revert(Some(
        &json!({"targetMessageID": "u2", "createdMessageID": "n"}),
    ));
    assert_eq!(
        ids(&active_messages(&messages, &branch, true)),
        ["u1", "a1", "n", "u3"]
    );
    let missing = Branch::from_revert(Some(&json!({"targetMessageID": "zz"})));
    assert_eq!(active_messages(&messages, &missing, true).len(), 6);
    assert_eq!(Branch::from_revert(Some(&Value::Null)), Branch::default());
}

#[test]
fn a_timeline_only_compaction_part_is_not_a_boundary() {
    assert!(!branch::is_boundary_part(
        &json!({"type": "compaction", "timelineStatus": "completed"})
    ));
    assert!(branch::is_boundary_part(
        &json!({"type": "compaction", "timelineStatus": ""})
    ));
    assert!(branch::is_boundary_part(
        &json!({"type": "compaction", "timelineStatus": "x", "compactBoundary": {}})
    ));
}

#[test]
fn entries_render_as_rust_canonical_messages() {
    let assistant = Entry::Message {
        message: json!({"role": "assistant", "content": [
            {"type": "reasoning", "text": "a", "providerOptions": {"anthropic": {"signature": "s"}}},
            {"type": "reasoning", "text": "b", "providerOptions": {"openai": {"itemId": "i", "reasoningEncryptedContent": "e"}}},
            {"type": "text", "text": "done"}],
            "toolCalls": [{"id": "c", "name": "Read", "input": {"b": 1.0, "a": "x"}}],
            "providerId": "p", "modelId": "m"}),
        metadata: None,
        tokens: None,
    };
    assert_eq!(
        assistant.canonical(),
        json!({"role": "assistant", "content": "done", "reasoning_content": "ab",
            "tool_calls": [{"id": "c", "type": "function", "function": {"name": "Read", "arguments": "{\"b\":1,\"a\":\"x\"}"}}],
            "_zcode_origin": {"provider": "p", "model": "m"},
            "_zcode_anthropic_thinking": [{"type": "thinking", "thinking": "a", "signature": "s"}],
            "_zcode_responses_reasoning": [{"type": "reasoning", "id": "i", "encrypted_content": "e",
                "summary": [{"type": "summary_text", "text": "b"}]}]})
    );
    let reminder = Entry::Attachment {
        source: "todo_reminder".into(),
        content: "Todos".into(),
    };
    assert_eq!(
        reminder.canonical(),
        json!({"role": "user", "content": "<system-reminder>\nTodos\n</system-reminder>", "_zcode_source": "todo_reminder"})
    );
    let tool = Entry::Message {
        message: json!({"role": "tool", "content": "x", "toolCallId": "c", "toolName": "Read", "isError": true}),
        metadata: None,
        tokens: None,
    };
    assert_eq!(
        tool.canonical(),
        json!({"role": "tool", "tool_call_id": "c", "content": "x", "_zcode_tool_failed": true})
    );
    let steer = Entry::Message {
        message: json!({"role": "user", "content": "go"}),
        metadata: Some(
            json!({"source": "legacy_synthetic", "inputPresentation": "task_notification"}),
        ),
        tokens: None,
    };
    assert_eq!(steer.canonical()["_zcode_source"], "legacy_synthetic");
}

#[test]
fn context_use_is_the_latest_post_compact_count_or_assistant_tokens() {
    let record = |info: Value, parts: Vec<Value>| Record { info, parts };
    let assistant = record(
        json!({"id": "a", "role": "assistant", "tokens": {"input": 90, "output": 10}}),
        vec![],
    );
    assert_eq!(
        super::context_used(std::slice::from_ref(&assistant)),
        Some(100)
    );
    let summary = record(
        json!({"id": "s", "role": "user", "summary": {"title": "Compact summary"}}),
        vec![
            json!({"type": "compaction", "compactBoundary": {"postCompactTokenCount": 30,
            "truePostCompactTokenCount": 40}}),
        ],
    );
    assert_eq!(super::context_used(&[assistant.clone(), summary]), Some(40));
    let total = record(
        json!({"id": "b", "role": "assistant", "tokens": {"total": 7, "input": 1}}),
        vec![],
    );
    assert_eq!(super::context_used(&[assistant, total]), Some(7));
    let empty: [Record; 0] = [];
    assert_eq!(super::context_used(&empty), None);
}
