//! 用量是观测（usage.md §2.5，工作线 F77）：用量行写不进去时只回退它自己，同批的消息、工具结果与回合结果照常提交，
//! 记一条带原因的 warn。现场：库跑过开发分支的迁移，`model_usage` 没有 `variant` 列。
use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::{Arc, Mutex};

use rust_cli_kernel_api::ids::{MessageId, SessionId, ToolUseId};
use rust_cli_kernel_api::message::{ContentBlock, Message, StopReason, UserMessage, UserOrigin};
use rust_cli_kernel_api::metering::{CallStatus, ModelCall, ToolCall};
use rust_cli_kernel_api::model::QuerySource;
use rust_cli_kernel_api::store::{Fact, SessionStore};
use serde_json::{Value, json};
use tracing::field::Field;
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

use crate::tests::{TestStore, assistant, batch, fixed_ids, selection, started, text, user};

/// 另一台电脑上的库：开发分支的迁移把 `variant` 改名成了 `reasoning_level`。
const RENAMED_VARIANT: &str = "alter table model_usage rename column variant to reasoning_level";

fn sid() -> SessionId {
    SessionId::new("sess_1")
}

fn model_call(message: &str) -> Fact {
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
            usage: None,
            retries: 0,
            error: None,
        },
    }
}

fn break_schema(t: &TestStore, sql: &str) {
    t.connection().execute_batch(sql).unwrap();
}

async fn commit(t: &TestStore, facts: Vec<Fact>) {
    t.store.commit(&sid(), batch(None, facts)).await.unwrap();
}

/// 冷读取的消息 id。
async fn stored(t: &TestStore) -> Vec<String> {
    let loaded = t.store.load(&sid()).await.unwrap();
    loaded
        .messages
        .iter()
        .map(|message| message.id().as_str().to_owned())
        .collect()
}

/// 当前线程上的 warn 事件，每条是字段名 → 值（`Debug` 的文字；消息在 `message`）。
#[derive(Clone, Default)]
struct Warnings(Arc<Mutex<Vec<BTreeMap<&'static str, String>>>>);

impl Warnings {
    fn take(&self) -> Vec<BTreeMap<&'static str, String>> {
        std::mem::take(&mut self.0.lock().unwrap())
    }
}

impl Subscriber for Warnings {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        if *event.metadata().level() != Level::WARN {
            return;
        }
        let mut fields = BTreeMap::new();
        event.record(&mut |field: &Field, value: &dyn Debug| {
            fields.insert(field.name(), format!("{value:?}"));
        });
        self.0.lock().unwrap().push(fields);
    }

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}

#[tokio::test]
async fn a_model_usage_row_that_cannot_be_written_keeps_the_reply() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    commit(&t, vec![user("msg_u1", "hi")]).await;
    break_schema(&t, RENAMED_VARIANT);
    commit(
        &t,
        vec![
            assistant("msg_a1", vec![text("hello")], None),
            model_call("msg_a1"),
        ],
    )
    .await;
    assert_eq!(stored(&t).await, ["msg_u1", "msg_a1"]);
    assert!(t.rows("select * from model_usage").is_empty());
    // 没有消息的计量（失败的请求）单独一批：照样提交，不报错。
    commit(&t, vec![model_call("msg_a2")]).await;
}

#[tokio::test]
async fn a_tool_usage_row_that_cannot_be_written_keeps_the_result() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    commit(&t, vec![user("msg_u1", "read it")]).await;
    let call = ContentBlock::ToolUse {
        id: ToolUseId::new("toolu_1"),
        name: "Read".into(),
        input: json!({"file_path": "/w/a.txt"}),
    };
    commit(&t, vec![assistant("msg_a1", vec![call], None)]).await;
    // 表不在：用量行与批末的保留期删除都写不进去。
    break_schema(&t, "drop table tool_usage");
    let result = Message::User(UserMessage {
        id: MessageId::new("msg_r1"),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new("toolu_1"),
            content: vec![rust_cli_kernel_api::message::ToolResultContent::Text {
                text: "alpha".into(),
            }],
            is_error: false,
        }],
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new("msg_a1"),
        },
    });
    let metered = ToolCall {
        tool_use: ToolUseId::new("toolu_1"),
        name: "Read".into(),
        started_at_ms: 3_000,
        completed_at_ms: 3_100,
        status: CallStatus::Completed,
        output_bytes: 5,
    };
    commit(
        &t,
        vec![
            Fact::MessageAppended { message: result },
            Fact::ToolCalled { call: metered },
        ],
    )
    .await;
    assert_eq!(stored(&t).await, ["msg_u1", "msg_a1", "msg_r1"]);
    let state = t.rows(
        "select json_extract(data, '$.state.status') from part
          where json_extract(data, '$.type') = 'tool'",
    );
    assert_eq!(state, vec![vec![json!("completed")]]);
}

#[tokio::test]
async fn a_turn_usage_row_that_cannot_be_written_keeps_the_outcome() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    commit(&t, vec![user("msg_u1", "hi")]).await;
    break_schema(&t, "drop table turn_usage");
    commit(
        &t,
        vec![Fact::TurnEnded {
            status: CallStatus::Completed,
        }],
    )
    .await;
    assert_eq!(
        t.rows("select last_outcome from session"),
        vec![vec![Value::from("completed")]]
    );
}

#[tokio::test]
async fn the_skipped_usage_is_logged_with_its_reason() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    commit(&t, vec![user("msg_u1", "hi")]).await;
    break_schema(&t, RENAMED_VARIANT);
    let warnings = Warnings::default();
    let facts = vec![
        assistant("msg_a1", vec![text("hello")], None),
        model_call("msg_a1"),
    ];
    // 同步调用提交（存储平时在阻塞线程里调它），订阅者只在当前线程。
    let mut conn = t.connection();
    let ids = fixed_ids(&t.source);
    let committed = tracing::subscriber::with_default(warnings.clone(), || {
        crate::facts::commit(&mut conn, &sid(), &batch(None, facts), &ids)
    });
    assert!(committed.is_ok(), "{committed:?}");
    let logged = warnings.take();
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert_eq!(logged[0]["event"], "\"usage.model.write.failed\"");
    assert_eq!(logged[0]["session"], "sess_1");
    assert!(
        logged[0]["reason"].contains("no column named variant"),
        "{logged:?}"
    );
}

#[tokio::test]
async fn a_failed_business_fact_still_rolls_back_the_whole_batch() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    commit(&t, vec![user("msg_u1", "hi")]).await;
    break_schema(&t, RENAMED_VARIANT);
    // 同一个 id 再追加是业务错误：用量行的 savepoint 不改变整批的原子性。
    let failed = t
        .store
        .commit(
            &sid(),
            batch(
                None,
                vec![
                    assistant("msg_a1", vec![text("hello")], None),
                    model_call("msg_a1"),
                    user("msg_u1", "again"),
                ],
            ),
        )
        .await;
    assert!(failed.is_err());
    assert_eq!(stored(&t).await, ["msg_u1"]);
}
