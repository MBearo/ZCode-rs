//! 压缩边界与摘要的记录形状、冷读取（docs/specs/rust-cli/compact.md §6）。
use rust_cli_kernel_api::ids::{MessageId, SessionId, ToolUseId};
use rust_cli_kernel_api::message::{
    AssistantMessage, AttachmentMessage, CompactBoundary, CompactTrigger, ContentBlock, Message,
    PreservedSegment, StopReason, SystemMessage, ToolResultContent, Usage, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::store::{Fact, SessionStore};
use serde_json::{Value, json};

use crate::tests::{TestStore, assistant, batch, started, text, user};

fn sid() -> SessionId {
    SessionId::new("sess_1")
}

fn appended(message: Message) -> Fact {
    Fact::MessageAppended { message }
}

fn boundary(preserved: Option<(&str, &str)>) -> Message {
    Message::System(SystemMessage::CompactBoundary(CompactBoundary {
        id: MessageId::new("msg_b"),
        trigger: CompactTrigger::Manual,
        pre_tokens: 1234,
        summary: MessageId::new("msg_s"),
        preserved: preserved.map(|(head, tail)| PreservedSegment {
            head: MessageId::new(head),
            anchor: MessageId::new("msg_s"),
            tail: MessageId::new(tail),
        }),
        discovered_tools: Vec::new(),
    }))
}

fn summary() -> Message {
    Message::User(UserMessage {
        id: MessageId::new("msg_s"),
        content: vec![ContentBlock::Text {
            text: "This session is being continued…".to_owned(),
        }],
        origin: UserOrigin::CompactSummary,
    })
}

fn file_attachment() -> Message {
    Message::Attachment(AttachmentMessage {
        id: MessageId::new("msg_f"),
        source: "file".to_owned(),
        text: "<system-reminder>\nCalled the Read tool\n</system-reminder>".to_owned(),
        data: json!({"filename": "/w/a.rs"}),
    })
}

fn json_rows(t: &TestStore, sql: &str) -> Vec<Value> {
    t.rows(sql)
        .into_iter()
        .map(|row| serde_json::from_str(row[0].as_str().unwrap()).unwrap())
        .collect()
}

#[tokio::test]
async fn boundary_and_summary_become_one_node_summary_message() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let usage = Usage {
        input_tokens: 10,
        output_tokens: 2,
        ..Usage::default()
    };
    for fact in [
        user("msg_u1", "first"),
        assistant("msg_a1", vec![text("one")], Some(usage)),
        user("msg_u2", "second"),
        assistant("msg_a2", vec![text("two")], Some(usage)),
    ] {
        t.store
            .commit(&sid(), batch(None, vec![fact]))
            .await
            .unwrap();
    }
    let facts = vec![
        appended(boundary(Some(("msg_a2", "msg_a2")))),
        appended(summary()),
        appended(file_attachment()),
    ];
    t.store.commit(&sid(), batch(None, facts)).await.unwrap();

    let info = &json_rows(&t, "select data from message where id = 'msg_s'")[0];
    assert_eq!(info["summary"]["title"], "Compact summary");
    assert_eq!(info["semantics"]["kind"], "compact_summary");
    assert_eq!(info["role"], "user");
    let parts = json_rows(
        &t,
        "select data from part where message_id = 'msg_s' order by sequence",
    );
    assert_eq!(parts[0]["type"], "text");
    assert_eq!(parts[0]["synthetic"], true);
    assert_eq!(parts[1]["type"], "compaction");
    assert_eq!(
        parts[1]["compactBoundary"],
        json!({"boundaryId": "msg_b", "trigger": "manual", "phase": "standalone_turn",
            "compactReason": "user_requested", "summarySource": "model",
            "preCompactTokenCount": 1234, "summarizedMessageCount": 3, "keptMessageCount": 1,
            "preservedSegment": {"headMessageId": "msg_a2", "anchorMessageId": "msg_s",
                "tailMessageId": "msg_a2"},
            "summaryMessageIds": ["msg_s"], "traceId": "sess_1"})
    );

    let loaded = t.store.load(&sid()).await.unwrap();
    let ids: Vec<&str> = loaded.messages.iter().map(|m| m.id().as_str()).collect();
    assert_eq!(ids, ["msg_b", "msg_s", "msg_a2", "msg_f"]);
    assert_eq!(loaded.messages[0], boundary(Some(("msg_a2", "msg_a2"))));
    assert_eq!(loaded.messages[1], summary());
    // 保留段的用量属于被替换的前缀。
    match &loaded.messages[2] {
        Message::Assistant(assistant) => assert_eq!(assistant.usage, None),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_tool_result_tail_maps_to_its_assistant_row() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let call = Message::Assistant(AssistantMessage {
        id: MessageId::new("msg_a1"),
        content: vec![ContentBlock::ToolUse {
            id: ToolUseId::new("toolu_1"),
            name: "Read".to_owned(),
            input: json!({"file_path": "/w/a.rs"}),
        }],
        model: "m".to_owned(),
        stop_reason: Some(StopReason::ToolUse),
        usage: None,
    });
    let result = Message::User(UserMessage {
        id: MessageId::new("msg_r1"),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new("toolu_1"),
            content: vec![ToolResultContent::Text {
                text: "ok".to_owned(),
            }],
            is_error: false,
        }],
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new("msg_a1"),
        },
    });
    for fact in [user("msg_u1", "read"), appended(call), appended(result)] {
        t.store
            .commit(&sid(), batch(None, vec![fact]))
            .await
            .unwrap();
    }
    let facts = vec![
        appended(boundary(Some(("msg_a1", "msg_r1")))),
        appended(summary()),
    ];
    t.store.commit(&sid(), batch(None, facts)).await.unwrap();
    let parts = json_rows(
        &t,
        "select data from part where message_id = 'msg_s' and json_extract(data, '$.type') = 'compaction'",
    );
    assert_eq!(
        parts[0]["compactBoundary"]["preservedSegment"]["tailMessageId"],
        "msg_a1"
    );
    let loaded = t.store.load(&sid()).await.unwrap();
    let ids: Vec<&str> = loaded.messages.iter().map(|m| m.id().as_str()).collect();
    assert_eq!(ids, ["msg_b", "msg_s", "msg_a1", "msg_r1"]);
}

#[tokio::test]
async fn a_summary_without_its_boundary_is_rejected() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    t.store
        .commit(&sid(), batch(None, vec![user("msg_u1", "hi")]))
        .await
        .unwrap();
    let error = t
        .store
        .commit(&sid(), batch(None, vec![appended(summary())]))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("has no boundary"), "{error}");
    assert!(
        t.rows("select id from message where id = 'msg_s'")
            .is_empty()
    );
}

/// 压缩前已加载的工具（compact.md §3.5）：存在 `compaction` part 的 `metadata.rustCli`，不进 strict 的
/// `compactBoundary`；冷读取还原到边界上。
#[tokio::test]
async fn the_tools_loaded_before_compaction_are_stored_beside_the_strict_payload() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    for fact in [
        user("msg_u1", "first"),
        assistant("msg_a1", vec![text("one")], None),
    ] {
        t.store
            .commit(&sid(), batch(None, vec![fact]))
            .await
            .unwrap();
    }
    let carried = vec!["WebFetch".to_owned(), "mcp__fixture__lookup".to_owned()];
    let mut with_tools = boundary(None);
    if let Message::System(SystemMessage::CompactBoundary(inner)) = &mut with_tools {
        inner.discovered_tools.clone_from(&carried);
    }
    let facts = vec![appended(with_tools.clone()), appended(summary())];
    t.store.commit(&sid(), batch(None, facts)).await.unwrap();

    let parts = json_rows(
        &t,
        "select data from part where message_id = 'msg_s' and json_extract(data, '$.type') = 'compaction'",
    );
    assert_eq!(
        parts[0]["metadata"]["rustCli"]["preCompactDiscoveredTools"],
        json!(carried)
    );
    assert!(
        parts[0]["compactBoundary"]
            .get("preCompactDiscoveredTools")
            .is_none()
    );

    let loaded = t.store.load(&sid()).await.unwrap();
    assert_eq!(loaded.messages[0], with_tools);
}
