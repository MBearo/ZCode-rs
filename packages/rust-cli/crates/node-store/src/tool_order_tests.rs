//! 工具结果的位置键（docs/specs/rust-cli/node-store.md §4.6，工作线 F81 R2）：结算时记下会话里最后一行，冷读取把
//! 结果插回那一行之后。
use rust_cli_kernel_api::ids::{MessageId, SessionId, ToolUseId};
use rust_cli_kernel_api::message::{
    AssistantMessage, AttachmentMessage, ContentBlock, Message, StopReason, ToolResultContent,
    UserMessage, UserOrigin,
};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::store::{Fact, SessionFork, SessionStore};
use serde_json::{Value, json};

use crate::tests::{TestStore, assistant, batch, selection, started, text, user};

fn sid() -> SessionId {
    SessionId::new("sess_1")
}

fn calls(id: &str, ids: &[&str]) -> Fact {
    Fact::MessageAppended {
        message: Message::Assistant(AssistantMessage {
            id: MessageId::new(id),
            content: ids
                .iter()
                .map(|call| ContentBlock::ToolUse {
                    id: ToolUseId::new(*call),
                    name: "Read".into(),
                    input: json!({"file_path": format!("/w/{call}")}),
                })
                .collect(),
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
                content: vec![ToolResultContent::Text {
                    text: format!("read {call}"),
                }],
                is_error: false,
            }],
            origin: UserOrigin::ToolResult {
                assistant: MessageId::new(owner),
            },
        }),
    }
}

/// PostToolUse 的附加上下文（system 角色的 attachment）。
fn context(id: &str, body: &str) -> Fact {
    Fact::MessageAppended {
        message: Message::Attachment(AttachmentMessage {
            id: MessageId::new(id),
            source: "hook_additional_context".into(),
            text: format!("<system-reminder>\n{body}\n</system-reminder>"),
            data: Value::Null,
        }),
    }
}

async fn commit_each(t: &TestStore, facts: &[Fact]) {
    for fact in facts {
        t.store
            .commit(&sid(), batch(None, vec![fact.clone()]))
            .await
            .unwrap();
    }
}

fn ids(messages: &[Message]) -> Vec<&str> {
    messages
        .iter()
        .map(|message| message.id().as_str())
        .collect()
}

/// 两个并行调用，结果按交出顺序提交、各自后面跟一条附加上下文：冷读取与实时的顺序相同。以前结果都排在
/// assistant 之后、附加上下文都排在结果之后，模型不支持 system turn 时两段上下文都并进最后一个结果。
#[tokio::test]
async fn results_read_back_after_the_rows_that_preceded_them() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    t.source.at(2000);
    let facts = vec![
        user("msg_ru_u1", "Read both"),
        calls("msg_rv_a1", &["toolu_1", "toolu_2"]),
        result("msg_rw_r2", "msg_rv_a1", "toolu_2"),
        context("msg_rx_c2", "ctx second"),
        result("msg_ry_r1", "msg_rv_a1", "toolu_1"),
        context("msg_rz_c1", "ctx first"),
        assistant("msg_s0_a2", vec![text("Done.")], None),
    ];
    commit_each(&t, &facts).await;
    let loaded = t.store.load(&sid()).await.unwrap();
    assert_eq!(
        ids(&loaded.messages),
        [
            "msg_ru_u1",
            "msg_rv_a1",
            "msg_rw_r2",
            "msg_rx_c2",
            "msg_ry_r1",
            "msg_rz_c1",
            "msg_s0_a2"
        ]
    );
    // 位置键是结算时最后一行：第一个结果之前是 assistant，第二个之前是第一段附加上下文。
    let keys = t.rows(
        "select json_extract(data, '$.callID'), json_extract(data, '$.metadata.rustCli.afterMessageId')
         from part where message_id = 'msg_rv_a1' and json_extract(data, '$.type') = 'tool'
         order by json_extract(data, '$.callID')",
    );
    assert_eq!(
        keys,
        vec![
            vec![json!("toolu_1"), json!("msg_rx_c2")],
            vec![json!("toolu_2"), json!("msg_rv_a1")],
        ]
    );
}

/// 没有位置键的旧数据（与 Node 写的结果）照旧：结果都紧跟 assistant，按提交顺序。
#[tokio::test]
async fn results_without_the_key_still_follow_their_assistant() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    t.source.at(2000);
    let facts = vec![
        user("msg_ru_u1", "Read both"),
        calls("msg_rv_a1", &["toolu_1", "toolu_2"]),
        result("msg_rw_r2", "msg_rv_a1", "toolu_2"),
        context("msg_rx_c2", "ctx second"),
        result("msg_ry_r1", "msg_rv_a1", "toolu_1"),
    ];
    commit_each(&t, &facts).await;
    t.connection()
        .execute(
            "update part set data = json_remove(data, '$.metadata.rustCli.afterMessageId')
             where message_id = 'msg_rv_a1'",
            [],
        )
        .unwrap();
    let loaded = t.store.load(&sid()).await.unwrap();
    assert_eq!(
        ids(&loaded.messages),
        [
            "msg_ru_u1",
            "msg_rv_a1",
            "msg_rw_r2",
            "msg_ry_r1",
            "msg_rx_c2"
        ]
    );
}

/// `/fork` 复制之后位置键换成新会话里那一行的 ID，分出的会话读回同样的顺序。以前位置键还指向原会话的行，分出的
/// 会话里找不到，结果又都排回 assistant 之后。
#[tokio::test]
async fn a_fork_keeps_the_results_in_place() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    t.source.at(2000);
    let facts = vec![
        user("msg_ru_u1", "Read both"),
        calls("msg_rv_a1", &["toolu_1", "toolu_2"]),
        result("msg_rw_r2", "msg_rv_a1", "toolu_2"),
        context("msg_rx_c2", "ctx second"),
        result("msg_ry_r1", "msg_rv_a1", "toolu_1"),
        context("msg_rz_c1", "ctx first"),
        assistant("msg_s0_a2", vec![text("Done.")], None),
    ];
    commit_each(&t, &facts).await;
    t.store
        .fork(SessionFork {
            parent: sid(),
            child: SessionId::new("sess_2"),
            created_at_ms: 36_000,
            through: MessageId::new("msg_s0_a2"),
            selection: selection(),
            mode: PermissionMode::Default,
            plan: false,
        })
        .await
        .unwrap();
    let loaded = t.store.load(&SessionId::new("sess_2")).await.unwrap();
    let shape: Vec<String> = loaded
        .messages
        .iter()
        .map(|message| match message {
            Message::User(user) => match user.content.first() {
                Some(ContentBlock::ToolResult { tool_use_id, .. }) => {
                    format!("result {tool_use_id}")
                }
                _ => "user".to_owned(),
            },
            Message::Assistant(_) => "assistant".to_owned(),
            Message::Attachment(attachment) => {
                attachment.text.lines().nth(1).unwrap_or("").to_owned()
            }
            Message::System(_) => "system".to_owned(),
        })
        .collect();
    assert_eq!(
        shape[..7],
        [
            "user",
            "assistant",
            "result toolu_2",
            "ctx second",
            "result toolu_1",
            "ctx first",
            "assistant"
        ]
    );
}
