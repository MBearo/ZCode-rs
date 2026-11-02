//! 本工程独有的 entry 的读回（spec node-store.md §4.7）：按种类的记录；旧版本写的操作意图（工作线 F119）与交互
//! （工作线 F130）不读、不再配对结果。
use rust_cli_base::js::json::stringify;
use rust_cli_kernel_api::ids::{MessageId, SessionId, ToolUseId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, StopReason, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::store::{Fact, SessionStore};
use serde_json::{Value, json};

use crate::tests::{TestStore, batch, started, user};

fn sid() -> SessionId {
    SessionId::new("sess_1")
}

fn calls(id: &str, calls: &[(&str, &str, &Value)]) -> Fact {
    let content = calls
        .iter()
        .map(|(call, name, input)| ContentBlock::ToolUse {
            id: ToolUseId::new(*call),
            name: (*name).into(),
            input: (*input).clone(),
        })
        .collect();
    Fact::MessageAppended {
        message: Message::Assistant(AssistantMessage {
            id: MessageId::new(id),
            content,
            model: "m".into(),
            stop_reason: Some(StopReason::ToolUse),
            usage: None,
        }),
    }
}

fn result(id: &str, owner: &str, call: &str) -> Fact {
    Fact::MessageAppended {
        message: Message::User(UserMessage {
            id: MessageId::new(id),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: ToolUseId::new(call),
                content: vec![ToolResultContent::Text { text: "ok".into() }],
                is_error: false,
            }],
            origin: UserOrigin::ToolResult {
                assistant: MessageId::new(owner),
            },
        }),
    }
}

/// 旧版本写的一条操作意图（工作线 F119 之前；`fingerprint` 是整份 JSON 时旧代码会按它配对工具结果）。
fn legacy(t: &TestStore, operation: &str, fingerprint: &str) {
    let data = json!({"operationId": operation, "fingerprint": fingerprint});
    t.connection()
        .execute(
            "insert into session_entry (id, session_id, type, time_created, time_updated, data)
             values (?1, 'sess_1', 'rust-cli/operation_intent', 1, 1, ?2)",
            [
                format!("sess_1:rust-cli/operation_intent:{operation}"),
                stringify(&data),
            ],
        )
        .unwrap();
}

/// 跨会话的 entry 按写入顺序读回（`SessionStore::entries`，cross-session.md §2.2）；别的种类不混进来。
#[tokio::test]
async fn rust_cli_entries_read_back_by_kind_in_order() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let entry = |kind: &str, id: &str, data: Value| Fact::Entry {
        kind: kind.to_owned(),
        id: id.to_owned(),
        data,
    };
    let facts = vec![
        entry("peer_message", "m2", json!({"msgId": "m2"})),
        entry(
            "peer_message",
            "m1",
            json!({"msgId": "m1", "held": {"cause": "mode-mismatch"}}),
        ),
        entry(
            "peer_resolved",
            "m1",
            json!({"msgId": "m1", "outcome": "denied"}),
        ),
    ];
    t.store.commit(&sid(), batch(None, facts)).await.unwrap();
    let messages = t.store.entries(&sid(), "peer_message").await.unwrap();
    assert_eq!(
        messages,
        [
            ("m2".to_owned(), json!({"msgId": "m2"})),
            (
                "m1".to_owned(),
                json!({"msgId": "m1", "held": {"cause": "mode-mismatch"}})
            ),
        ]
    );
    assert_eq!(
        t.store
            .entries(&sid(), "peer_resolved")
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        t.store
            .entries(&sid(), "peer_receipt")
            .await
            .unwrap()
            .is_empty()
    );
}

/// 旧版本写的意图（工作线 F119 之前，每次工具调用一行意图、一行结果）：留在库里不读；工具结果落库时不再写
/// `operation_result`，冷读取照常。
#[tokio::test]
async fn old_intent_rows_are_left_alone_and_no_results_are_written() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let a = json!({"file_path": "/w/a"});
    legacy(
        &t,
        "op_old",
        &stringify(&json!({"tool": "Read", "input": a})),
    );
    let facts = vec![
        user("msg_ru_u1", "Go"),
        calls("msg_rv_a1", &[("toolu_1", "Read", &a)]),
        result("msg_rw_r1", "msg_rv_a1", "toolu_1"),
    ];
    t.store.commit(&sid(), batch(None, facts)).await.unwrap();
    let results: i64 = t
        .connection()
        .query_row(
            "select count(*) from session_entry where type = 'rust-cli/operation_result'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(results, 0);
    t.store.load(&sid()).await.unwrap();
}
