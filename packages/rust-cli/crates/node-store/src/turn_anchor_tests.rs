//! 批次的回合锚点（node-store.md §4，修复线 F6，审查 R5-3）：锚点给出的当前轮优先，写进库的列与按库推导时相同。
use std::collections::HashMap;

use rust_cli_kernel_api::ids::{CommandId, MessageId, OwnerEpoch, SessionId, ToolUseId, TurnId};
use rust_cli_kernel_api::message::{ContentBlock, Message, StopReason, ToolResultContent, Usage};
use rust_cli_kernel_api::message::{UserMessage, UserOrigin};
use rust_cli_kernel_api::metering::{CallStatus, ModelCall, ToolCall};
use rust_cli_kernel_api::model::QuerySource;
use rust_cli_kernel_api::store::{Fact, FactBatch, SessionStore, TurnAnchor};
use rust_cli_kernel_api::user_input::{INTERRUPT_TEXTS, bash_input};
use serde_json::Value;

use crate::tests::{TestStore, assistant, selection, started, text, user};

fn sid() -> SessionId {
    SessionId::new("sess_1")
}

fn anchor(prompt: &str, turn: &str) -> TurnAnchor {
    TurnAnchor {
        prompt: MessageId::new(prompt),
        turn: Some(TurnId::new(turn)),
    }
}

fn anchored(facts: Vec<Fact>, turn: Option<TurnAnchor>) -> FactBatch {
    FactBatch {
        owner_epoch: OwnerEpoch::LOCAL,
        command: None::<CommandId>,
        facts,
        turn,
    }
}

fn meta(id: &str, text: &str) -> Fact {
    Fact::MessageAppended {
        message: Message::User(UserMessage {
            id: MessageId::new(id),
            content: vec![ContentBlock::Text { text: text.into() }],
            origin: UserOrigin::Meta {
                source: "task-notification".into(),
            },
        }),
    }
}

fn model(message: &str) -> Fact {
    Fact::ModelCalled {
        call: ModelCall {
            message: MessageId::new(message),
            source: QuerySource::MainThread,
            selection: selection(),
            started_at_ms: 2_000,
            first_token_at_ms: Some(2_100),
            completed_at_ms: 2_500,
            status: CallStatus::Completed,
            stop_reason: Some(StopReason::EndTurn),
            tool_calls: 0,
            usage: Some(Usage {
                input_tokens: 3,
                output_tokens: 2,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 1,
            }),
            retries: 0,
            error: None,
        },
    }
}

fn tool(id: &str) -> Fact {
    Fact::ToolCalled {
        call: ToolCall {
            tool_use: ToolUseId::new(id),
            name: "Read".into(),
            started_at_ms: 3_000,
            completed_at_ms: 3_200,
            status: CallStatus::Completed,
            output_bytes: 2,
        },
    }
}

fn result(id: &str, call: &str, assistant: &str) -> Fact {
    Fact::MessageAppended {
        message: Message::User(UserMessage {
            id: MessageId::new(id),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: ToolUseId::new(call),
                content: vec![ToolResultContent::Text { text: "ok".into() }],
                is_error: false,
            }],
            origin: UserOrigin::ToolResult {
                assistant: MessageId::new(assistant),
            },
        }),
    }
}

fn info(t: &TestStore, id: &str) -> Value {
    let rows = t.rows(&format!("select data from message where id = '{id}'"));
    serde_json::from_str(rows[0][0].as_str().unwrap()).unwrap()
}

/// 以前 node-store 不看锚点，按库里最后一条真实输入（这里是 Node 续写的 `msg_u2`）推导。
#[tokio::test]
async fn the_batch_anchor_names_the_turn_instead_of_the_latest_stored_input() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let first = anchor("msg_u1", "turn_actor_1");
    t.store
        .commit(
            &sid(),
            anchored(vec![user("msg_u1", "one")], Some(first.clone())),
        )
        .await
        .unwrap();
    assert_eq!(info(&t, "msg_u1")["anchor"]["turnId"], "turn_actor_1");
    // 别的写者在库里留下一条更晚的输入；这一批仍属于锚点所指的轮次。
    t.store
        .commit(&sid(), anchored(vec![user("msg_u2", "two")], None))
        .await
        .unwrap();
    let facts = vec![
        assistant("msg_a1", vec![text("ok")], None),
        model("msg_a1"),
        tool("toolu_1"),
    ];
    t.store
        .commit(&sid(), anchored(facts, Some(first)))
        .await
        .unwrap();
    let reply = info(&t, "msg_a1");
    assert_eq!(reply["parentID"], "msg_u1");
    assert_eq!(reply["anchor"]["turnId"], "turn_actor_1");
    let usage = t.rows("select turn_id, parent_user_message_id from model_usage");
    assert_eq!(usage[0][0], "turn_actor_1");
    assert_eq!(usage[0][1], "msg_u1");
    assert_eq!(
        t.rows("select turn_id from tool_usage")[0][0],
        "turn_actor_1"
    );
}

/// 锚点所指的输入在同一批里：写到它之前的事实仍属于上一轮（与按库推导相同）。
#[tokio::test]
async fn facts_before_the_anchored_prompt_in_its_batch_keep_the_previous_turn() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    t.store
        .commit(
            &sid(),
            anchored(
                vec![user("msg_u1", "one")],
                Some(anchor("msg_u1", "turn_a")),
            ),
        )
        .await
        .unwrap();
    let facts = vec![meta("msg_m1", "before"), user("msg_u2", "two")];
    t.store
        .commit(&sid(), anchored(facts, Some(anchor("msg_u2", "turn_b"))))
        .await
        .unwrap();
    assert_eq!(info(&t, "msg_m1")["anchor"]["turnId"], "turn_a");
    assert_eq!(info(&t, "msg_u2")["anchor"]["turnId"], "turn_b");
}

#[tokio::test]
async fn a_cold_load_returns_the_current_turn() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    assert_eq!(t.store.load(&sid()).await.unwrap().turn, None);
    t.store
        .commit(
            &sid(),
            anchored(
                vec![user("msg_u1", "one")],
                Some(anchor("msg_u1", "turn_a")),
            ),
        )
        .await
        .unwrap();
    t.store
        .commit(
            &sid(),
            anchored(
                vec![assistant("msg_a1", vec![text("ok")], None)],
                Some(anchor("msg_u1", "turn_a")),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        t.store.load(&sid()).await.unwrap().turn,
        Some(anchor("msg_u1", "turn_a"))
    );
}

/// 一个会话的批次：人类输入、工具往返、通知、开回合的通知、同一批里先于输入的合成消息、中断与 bash 模式。
fn script() -> Vec<Vec<Fact>> {
    let tool_use = ContentBlock::ToolUse {
        id: ToolUseId::new("toolu_1"),
        name: "Read".into(),
        input: serde_json::json!({"file_path": "/w/a.txt"}),
    };
    let ended = || Fact::TurnEnded {
        status: CallStatus::Completed,
    };
    let bash_output = "<bash-stdout>a</bash-stdout><bash-stderr></bash-stderr>";
    vec![
        vec![user("msg_u1", "one")],
        vec![assistant("msg_a1", vec![tool_use], None), model("msg_a1")],
        vec![result("msg_r1", "toolu_1", "msg_a1"), tool("toolu_1")],
        vec![meta("msg_m1", "notice at a tool boundary")],
        vec![
            assistant("msg_a2", vec![text("done")], None),
            model("msg_a2"),
        ],
        vec![ended()],
        vec![meta("msg_n1", "a background notice opens a turn")],
        vec![assistant("msg_a3", vec![text("ok")], None), model("msg_a3")],
        vec![ended()],
        vec![meta("msg_m2", "before the prompt"), user("msg_u2", "two")],
        vec![
            assistant("msg_a4", vec![text("two")], None),
            model("msg_a4"),
        ],
        vec![user("msg_i1", INTERRUPT_TEXTS[0])],
        vec![ended()],
        vec![
            user("msg_b1", &bash_input("ls")),
            user("msg_b2", bash_output),
        ],
        vec![ended()],
    ]
}

/// 每张表的行，轮次 ID 按首次出现的顺序换成编号（锚点的轮次与库里生成的不同）。
fn rows(t: &TestStore) -> Vec<Vec<Value>> {
    let mut turns: HashMap<String, usize> = HashMap::new();
    let mut name = |value: &Value| match value.as_str() {
        Some(turn) if turn.starts_with("turn_") => {
            let next = turns.len();
            Value::from(format!(
                "turn#{}",
                turns.entry(turn.to_owned()).or_insert(next)
            ))
        }
        _ => value.clone(),
    };
    let mut out = Vec::new();
    for row in t.rows("select id, data from message order by sequence") {
        let data: Value = serde_json::from_str(row[1].as_str().unwrap()).unwrap();
        out.push(vec![
            row[0].clone(),
            data["parentID"].clone(),
            name(&data["anchor"]["turnId"]),
        ]);
    }
    for table in ["model_usage", "tool_usage", "turn_usage"] {
        for row in t.rows(&format!("select * from {table} order by rowid")) {
            out.push(row.iter().map(&mut name).collect());
        }
    }
    out
}

#[tokio::test]
async fn anchored_batches_write_the_same_rows_as_batches_the_store_derives() {
    let derived = TestStore::new();
    let with_anchor = TestStore::new();
    for t in [&derived, &with_anchor] {
        started(t, "sess_1").await;
    }
    let mut current: Option<TurnAnchor> = None;
    let mut turns = 0;
    for facts in script() {
        derived
            .store
            .commit(&sid(), anchored(facts.clone(), None))
            .await
            .unwrap();
        let next = TurnAnchor::after(current.as_ref(), &facts, || {
            turns += 1;
            TurnId::new(format!("turn_actor_{turns}"))
        });
        with_anchor
            .store
            .commit(&sid(), anchored(facts, next.clone()))
            .await
            .unwrap();
        current = next;
    }
    assert_eq!(rows(&derived), rows(&with_anchor));
    // 12 条消息、4 个请求、1 个工具调用、3 个轮次。
    assert_eq!(rows(&derived).len(), 20);
}
