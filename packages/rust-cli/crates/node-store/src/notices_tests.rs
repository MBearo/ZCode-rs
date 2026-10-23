//! 合成 user 消息：attachment、meta、用户中断（spec node-store.md §4.5）。
use rust_cli_base::js::json::stringify;
use rust_cli_kernel_api::ids::{MessageId, SessionId};
use rust_cli_kernel_api::message::{
    AttachmentMessage, ContentBlock, Message, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::store::{Fact, SessionStore, StoreError};
use serde_json::{Value, json};

use crate::tests::{TestStore, assistant, batch, started, text, user};

fn sid() -> SessionId {
    SessionId::new("sess_1")
}

fn attachment(id: &str, source: &str, body: &str, data: Value) -> Fact {
    Fact::MessageAppended {
        message: Message::Attachment(AttachmentMessage {
            id: MessageId::new(id),
            source: source.into(),
            text: format!("<system-reminder>\n{body}\n</system-reminder>"),
            data,
        }),
    }
}

fn user_with(id: &str, content: Vec<ContentBlock>, origin: UserOrigin) -> Fact {
    Fact::MessageAppended {
        message: Message::User(UserMessage {
            id: MessageId::new(id),
            content,
            origin,
        }),
    }
}

fn messages(facts: &[Fact]) -> Vec<Message> {
    facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::MessageAppended { message } => Some(message.clone()),
            _ => None,
        })
        .collect()
}

/// 一条消息行与它的 part 行（`data` 列）。
fn stored(t: &TestStore, id: &str) -> (String, Vec<String>) {
    let text = |rows: Vec<Vec<Value>>| -> Vec<String> {
        rows.into_iter()
            .map(|row| row[0].as_str().unwrap().to_owned())
            .collect()
    };
    let message = text(t.rows(&format!("select data from message where id = '{id}'")));
    let parts = text(t.rows(&format!(
        "select data from part where message_id = '{id}' order by sequence"
    )));
    (message[0].clone(), parts)
}

/// attachment 写成 Node `persistRuntimeReminderMessage` 形状的 model-only 合成消息，`data` 存在
/// part 的 `metadata.rustCli`；冷加载原样还原，user 角色与 system 角色的来源都一样。
#[tokio::test]
async fn attachments_are_synthetic_reminders_that_read_back_unchanged() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    t.source.at(2000);
    let facts = vec![
        user("msg_ru_u1", "Hello"),
        attachment(
            "msg_rw_t1",
            "date",
            "Today's date is 2026-10-17.",
            json!({"date": "2026-10-17"}),
        ),
        attachment("msg_rx_t2", "remote_session_change", "x", Value::Null),
        // 只有数据、不渲染文本的 attachment（发给模型时为空）。
        Fact::MessageAppended {
            message: Message::Attachment(AttachmentMessage {
                id: MessageId::new("msg_ry_t3"),
                source: "max_turns_reached".into(),
                text: String::new(),
                data: json!({"maxTurns": 3, "turnCount": 3}),
            }),
        },
        assistant("msg_s0_a1", vec![text("Hi")], None),
    ];
    for fact in &facts {
        t.store
            .commit(&sid(), batch(None, vec![fact.clone()]))
            .await
            .unwrap();
    }
    let loaded = t.store.load(&sid()).await.unwrap();
    assert_eq!(loaded.messages, messages(&facts));

    let (message, parts) = stored(&t, "msg_rw_t1");
    let metadata = json!({"source": "date", "visibility": "model-only"});
    assert_eq!(
        message,
        stringify(
            &json!({"role": "user", "time": {"created": 1004}, "agent": "zcode-agent",
            "metadata": metadata,
            "modelSelection": {"providerId": "p", "modelId": "m"},
            "anchor": {"turnId": "turn_00000000-0000-4000-8000-000000000001", "origin": "synthetic"},
            "semantics": {"origin": "agent_runtime", "kind": "system_reminder", "source": "date",
                "uiVisibility": "hidden", "providerVisibility": "visible",
                "transcriptVisibility": "hidden"},
            "synthetic": true, "tools": {}, "visibility": "model-only",
            "model": {"providerID": "p", "modelID": "m"}})
        )
    );
    assert_eq!(
        parts,
        vec![stringify(&json!({"type": "text",
            "text": "<system-reminder>\nToday's date is 2026-10-17.\n</system-reminder>",
            "synthetic": true, "time": {"start": 1004, "end": 1004},
            "metadata": {"source": "date", "visibility": "model-only",
                "rustCli": {"message": "attachment", "data": {"date": "2026-10-17"}}}}))]
    );
    // attachment 不是真实输入：assistant 的父消息仍是用户输入。
    let parent =
        t.rows("select json_extract(data, '$.parentID') from message where id = 'msg_s0_a1'");
    assert_eq!(parent, vec![vec![Value::from("msg_ru_u1")]]);
}

/// meta 消息与中断消息：model-only 合成消息，冷加载还原原来的来源；中断不改标题、不开新轮。
#[tokio::test]
async fn meta_and_interrupt_messages_keep_their_origin() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    t.source.at(2000);
    let interrupt = "[Request interrupted by user]";
    let facts = vec![
        user("msg_ru_u1", "Hello"),
        assistant("msg_rv_a1", vec![text("Hi")], None),
        user_with("msg_rw_i1", vec![text(interrupt)], UserOrigin::Human),
        user_with(
            "msg_rx_m1",
            vec![text("Output token limit hit."), text("Resume directly.")],
            UserOrigin::Meta {
                source: "max_output_tokens_recovery".into(),
            },
        ),
        assistant("msg_ry_a2", vec![text("Again")], None),
    ];
    t.store
        .commit(&sid(), batch(None, facts.clone()))
        .await
        .unwrap();
    let loaded = t.store.load(&sid()).await.unwrap();
    assert_eq!(loaded.messages, messages(&facts));
    assert_eq!(loaded.meta.title, "Hello");

    let rows = t.rows(
        "select json_extract(data, '$.semantics.origin'), json_extract(data, '$.visibility'),
           json_extract(data, '$.metadata.source') from message
         where id in ('msg_rw_i1', 'msg_rx_m1') order by sequence",
    );
    let row = |source: &str| -> Vec<Value> {
        vec!["agent_runtime".into(), "model-only".into(), source.into()]
    };
    assert_eq!(
        rows,
        vec![row("interrupt"), row("max_output_tokens_recovery")]
    );
    assert_eq!(stored(&t, "msg_rx_m1").1.len(), 2);
    let parent =
        t.rows("select json_extract(data, '$.parentID') from message where id = 'msg_ry_a2'");
    assert_eq!(parent, vec![vec![Value::from("msg_ru_u1")]]);
}

#[tokio::test]
async fn meta_messages_with_images_are_refused() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let image = ContentBlock::Image {
        source: rust_cli_kernel_api::message::ImageSource::Base64 {
            media_type: "image/png".into(),
            data: "AA==".into(),
        },
    };
    let fact = user_with(
        "msg_ru_m1",
        vec![image],
        UserOrigin::Meta {
            source: "hook".into(),
        },
    );
    let result = t.store.commit(&sid(), batch(None, vec![fact])).await;
    assert!(
        matches!(result, Err(StoreError::Unsupported { .. })),
        "{result:?}"
    );
    assert!(t.rows("select id from message").is_empty());
}
