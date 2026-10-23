//! 操作意图与待回答的交互（spec node-store.md §4.7）。意图存 fingerprint 的摘要，旧记录存的整份 JSON 照旧判同 / 判异
//! （修复线 F6，审查 R5-12）。
use rust_cli_base::js::json::stringify;
use rust_cli_kernel_api::ids::{InteractionId, MessageId, OperationId, SessionId, ToolUseId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, StopReason, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::orchestration::InteractionRequest;
use rust_cli_kernel_api::store::{
    Fact, OpenOperation, PendingInteraction, SessionStore, StoreError,
};
use serde_json::{Value, json};

use crate::tests::{TestStore, batch, started, user};

fn sid() -> SessionId {
    SessionId::new("sess_1")
}

/// query 执行器的 fingerprint（`json!({"tool": name, "input": input}).to_string()`）。
fn fingerprint(tool: &str, input: &Value) -> String {
    json!({"tool": tool, "input": input}).to_string()
}

fn intent(operation: &str, tool: &str, input: &Value) -> Fact {
    Fact::OperationIntent {
        operation: OperationId::new(operation),
        fingerprint: fingerprint(tool, input),
    }
}

/// 存下的摘要（修复线 F6）：`sha256:` + `JSON.stringify({tool, input})` 的 SHA-256。
fn digest(tool: &str, input: &Value) -> String {
    use sha2::Digest;
    let text = stringify(&json!({"tool": tool, "input": input}));
    format!("sha256:{:x}", sha2::Sha256::digest(text.as_bytes()))
}

fn open(operation: &str, tool: &str, input: &Value) -> OpenOperation {
    OpenOperation {
        operation: OperationId::new(operation),
        fingerprint: digest(tool, input),
    }
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

/// 意图先于 assistant 落库（工具在流式响应中开工）；结果按 fingerprint 结掉对应的意图，没有结果的
/// 留在 `open_operations`。同一 fingerprint 的并发调用各结掉一个。
#[tokio::test]
async fn intents_without_results_are_open_after_a_cold_load() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let (a, b, bash) = (
        json!({"file_path": "/w/a"}),
        json!({"file_path": "/w/b"}),
        json!({"command": "make"}),
    );
    let facts = vec![
        user("msg_ru_u1", "Go"),
        intent("op_1", "Read", &a),
        intent("op_2", "Read", &b),
        intent("op_3", "Read", &b),
        intent("op_4", "Bash", &bash),
        calls(
            "msg_rv_a1",
            &[
                ("toolu_1", "Read", &a),
                ("toolu_2", "Read", &b),
                ("toolu_3", "Read", &b),
                ("toolu_4", "Bash", &bash),
            ],
        ),
        result("msg_rw_r2", "msg_rv_a1", "toolu_2"),
        result("msg_rx_r3", "msg_rv_a1", "toolu_3"),
    ];
    t.store.commit(&sid(), batch(None, facts)).await.unwrap();
    let loaded = t.store.load(&sid()).await.unwrap();
    assert_eq!(
        loaded.open_operations,
        vec![open("op_1", "Read", &a), open("op_4", "Bash", &bash)]
    );
    let row = t.rows(
        "select id, type, data from session_entry where type like 'rust-cli/operation_%'
         order by rowid limit 1",
    );
    let data = json!({"operationId": "op_1", "fingerprint": digest("Read", &a)});
    assert_eq!(
        row,
        vec![vec![
            Value::from("sess_1:rust-cli/operation_intent:op_1"),
            Value::from("rust-cli/operation_intent"),
            Value::from(stringify(&data)),
        ]]
    );
    let settled = t.rows(
        "select data from session_entry where type = 'rust-cli/operation_result' order by rowid",
    );
    let result = json!({"operationId": "op_2", "toolUseId": "toolu_2", "status": "completed"});
    assert_eq!(settled[0], vec![Value::from(stringify(&result))]);
    assert_eq!(settled.len(), 2);
}

/// 同一个 operationId 再登记：fingerprint 相同不变，不同则拒绝（README §6 规则 3）。
#[tokio::test]
async fn an_operation_id_keeps_its_fingerprint() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let input = json!({"file_path": "/w/a"});
    let first = vec![intent("op_1", "Read", &input)];
    t.store
        .commit(&sid(), batch(None, first.clone()))
        .await
        .unwrap();
    t.store.commit(&sid(), batch(None, first)).await.unwrap();
    let other = vec![intent("op_1", "Read", &json!({"file_path": "/w/b"}))];
    let refused = t.store.commit(&sid(), batch(None, other)).await;
    assert!(
        matches!(refused, Err(StoreError::Storage { .. })),
        "{refused:?}"
    );
    let loaded = t.store.load(&sid()).await.unwrap();
    assert_eq!(loaded.open_operations, vec![open("op_1", "Read", &input)]);
}

fn entry(kind: &str, id: &str, data: Value) -> Fact {
    Fact::Entry {
        kind: kind.into(),
        id: id.into(),
        data,
    }
}

/// 会话 actor 的两种交互 entry：问过、没有回答（也没有取消）的交互在冷读取后仍待回答。
#[tokio::test]
async fn unanswered_interactions_are_pending_after_a_cold_load() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let permission = InteractionRequest::Permission {
        tool_use: ToolUseId::new("toolu_1"),
        tool: "Bash".into(),
        input: json!({"command": "rm -rf build"}),
        message: "Allow?".into(),
        suggestions: Vec::new(),
        asked_by: None,
    };
    let plan = InteractionRequest::PlanApproval {
        tool_use: ToolUseId::new("toolu_2"),
        plan: "Ship it".into(),
    };
    let question = InteractionRequest::Question {
        tool_use: ToolUseId::new("toolu_3"),
        questions: json!([{"question": "Which?"}]),
    };
    let value = |request: &InteractionRequest| serde_json::to_value(request).unwrap();
    let facts = vec![
        entry("pending_interaction", "ia_1", value(&permission)),
        entry("pending_interaction", "ia_2", value(&plan)),
        entry("pending_interaction", "ia_3", value(&question)),
        entry("interaction_resolved", "ia_1", json!({"decision": "allow"})),
        entry("interaction_resolved", "ia_3", json!({"cancelled": true})),
    ];
    t.store.commit(&sid(), batch(None, facts)).await.unwrap();
    let loaded = t.store.load(&sid()).await.unwrap();
    assert_eq!(
        loaded.pending_interactions,
        vec![PendingInteraction {
            interaction: InteractionId::new("ia_2"),
            request: plan,
        }]
    );
}

/// 修复线 F6 之前写的意图（`fingerprint` 是整份 JSON）：工具结果照旧按 JSON 结掉它；同一 operationId 再登记时与
/// 新到的 JSON 原文比。
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

#[tokio::test]
async fn legacy_json_intents_settle_and_keep_their_fingerprint() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let (a, b) = (json!({"file_path": "/w/a"}), json!({"file_path": "/w/b"}));
    legacy(&t, "op_old", &fingerprint("Read", &a));
    legacy(&t, "op_other", &fingerprint("Read", &b));
    // 同一个 operationId：同样的 JSON 照旧接受，不同的拒绝。
    let same = vec![intent("op_old", "Read", &a)];
    t.store.commit(&sid(), batch(None, same)).await.unwrap();
    let other = vec![intent("op_old", "Read", &b)];
    assert!(t.store.commit(&sid(), batch(None, other)).await.is_err());
    let facts = vec![
        user("msg_ru_u1", "Go"),
        intent("op_new", "Read", &a),
        calls(
            "msg_rv_a1",
            &[("toolu_1", "Read", &a), ("toolu_2", "Read", &a)],
        ),
        result("msg_rw_r1", "msg_rv_a1", "toolu_1"),
        result("msg_rx_r2", "msg_rv_a1", "toolu_2"),
    ];
    t.store.commit(&sid(), batch(None, facts)).await.unwrap();
    // 最早的未结意图先结：旧记录与新记录各结一个；`/w/b` 的旧记录对不上，仍未结。
    let loaded = t.store.load(&sid()).await.unwrap();
    assert_eq!(
        loaded.open_operations,
        vec![OpenOperation {
            operation: OperationId::new("op_other"),
            fingerprint: fingerprint("Read", &b),
        }]
    );
}

/// 存的是摘要（以前是整份输入：Write 的全文在 tool part 与意图里各存一份）。
#[tokio::test]
async fn an_intent_stores_a_digest_of_its_input() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let content = "x".repeat(20_000);
    let input = json!({"file_path": "/w/big.txt", "content": content});
    t.store
        .commit(&sid(), batch(None, vec![intent("op_1", "Write", &input)]))
        .await
        .unwrap();
    let rows = t.rows("select data from session_entry where type = 'rust-cli/operation_intent'");
    let data: Value = serde_json::from_str(rows[0][0].as_str().unwrap()).unwrap();
    assert_eq!(data["fingerprint"], digest("Write", &input));
    assert!(rows[0][0].as_str().unwrap().len() < 200);
    // 数字按 JS 写出后再比：`1.0` 与读回的 `1` 是同一个输入（以前的值比较认为不同，意图一直不结）。
    let float = json!({"file_path": "/w/a", "limit": 1.0});
    let facts = vec![
        user("msg_ru_u1", "Go"),
        intent("op_2", "Read", &float),
        calls("msg_rv_a1", &[("toolu_1", "Read", &float)]),
        result("msg_rw_r1", "msg_rv_a1", "toolu_1"),
    ];
    t.store.commit(&sid(), batch(None, facts)).await.unwrap();
    let loaded = t.store.load(&sid()).await.unwrap();
    assert_eq!(loaded.open_operations, vec![open("op_1", "Write", &input)]);
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
