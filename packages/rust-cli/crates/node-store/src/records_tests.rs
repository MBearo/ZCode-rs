//! 事实 → Node 记录逐字节比对：`fixtures/node-records.json` 由
//! `scripts/rust-cli/node-store-fixtures.mjs` 用 Node 的真实代码在固定时钟与 uuid 下生成；
//! 这里用 NodeStore 提交同一组事实，各表的行必须相同（rust-cli 自己的 `rust-cli/*` entry 除外）。
//! 两边的操作序列一一对应，改一边时同步改另一边。
use rust_cli_kernel_api::ids::{MessageId, SessionId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, StopReason, Usage, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::store::{Fact, SessionStore};
use serde_json::Value;

use crate::tests::{TestStore, batch, meta, selection, text, user};

const SESSION: &str = "sess_records";

fn fixture() -> Value {
    serde_json::from_str(include_str!("../fixtures/node-records.json")).unwrap()
}

fn reply(id: &str, content: Vec<ContentBlock>, usage: Option<Usage>, stop: StopReason) -> Fact {
    Fact::MessageAppended {
        message: Message::Assistant(AssistantMessage {
            id: MessageId::new(id),
            content,
            model: "m".into(),
            stop_reason: Some(stop),
            usage,
        }),
    }
}

async fn replay(t: &TestStore) {
    let session = SessionId::new(SESSION);
    let commit = |now: u64, command: Option<&'static str>, facts: Vec<Fact>| {
        t.source.at(now);
        let session = session.clone();
        async move {
            t.store
                .commit(&session, batch(command, facts))
                .await
                .unwrap()
        }
    };
    t.source.at(1000);
    t.store.create(meta(SESSION, "/w/repo", "")).await.unwrap();
    let mode = |mode| Fact::ModeChanged { mode };
    let selected = Fact::ModelSelected {
        selection: selection(),
    };
    commit(
        1001,
        None,
        vec![selected, mode(PermissionMode::AcceptEdits)],
    )
    .await;
    commit(
        1002,
        Some("cmd_1"),
        vec![user("msg_ru_u1", "Hello  \"world\"\n\ttab")],
    )
    .await;
    let thinking = ContentBlock::Thinking {
        thinking: "Let me think".into(),
        signature: Some("sig-1".into()),
    };
    let redacted = ContentBlock::RedactedThinking { data: "abc".into() };
    let usage = Usage {
        input_tokens: 10,
        output_tokens: 5,
        cache_creation_input_tokens: 3,
        cache_read_input_tokens: 2,
    };
    let first = vec![thinking, redacted, text("Hi there, "), text("done.")];
    let first = reply("msg_rv_a1", first, Some(usage), StopReason::EndTurn);
    commit(1010, None, vec![first]).await;
    commit(1500, None, vec![mode(PermissionMode::Plan)]).await;
    commit(2000, None, vec![user("msg_1jk_u2", "Second")]).await;
    let second = reply(
        "msg_1jl_a2",
        vec![text("Done")],
        None,
        StopReason::MaxTokens,
    );
    commit(2005, None, vec![second]).await;
    let title = Fact::TitleChanged {
        title: "Renamed".into(),
    };
    commit(2100, None, vec![title]).await;
}

#[tokio::test]
async fn text_turns_are_stored_like_node_byte_for_byte() {
    let t = TestStore::new();
    replay(&t).await;
    let fixture = fixture();
    // 会话行的 version 是入口给的版本：测试库给的是夹具的版本（`FIXTURE_VERSION`），整行逐字节比对。
    let expected_session = fixture["tables"]["session"].clone();
    let tables = [
        ("session", "select * from session", expected_session),
        (
            "message",
            "select * from message order by sequence",
            fixture["tables"]["message"].clone(),
        ),
        (
            "part",
            "select * from part order by message_id, sequence",
            fixture["tables"]["part"].clone(),
        ),
        (
            "session_entry",
            "select * from session_entry where type not like 'rust-cli/%' order by id",
            fixture["tables"]["session_entry"].clone(),
        ),
    ];
    for (name, sql, expected) in tables {
        let rows = Value::from(t.rows(sql));
        assert_eq!(rows, expected, "{name}");
    }
}

/// 同一份记录冷读取回内核消息：文本块拼成一个（Node 每步只有一个 text part），其余与提交时相同。
#[tokio::test]
async fn the_node_records_read_back_as_kernel_messages() {
    let t = TestStore::new();
    replay(&t).await;
    let loaded = t.store.load(&SessionId::new(SESSION)).await.unwrap();
    let user = |id: &str, text: &str| {
        Message::User(UserMessage {
            id: MessageId::new(id),
            content: vec![ContentBlock::Text { text: text.into() }],
            origin: UserOrigin::Human,
        })
    };
    let assistant = |id: &str, content, usage, stop| {
        Message::Assistant(AssistantMessage {
            id: MessageId::new(id),
            content,
            model: "m".into(),
            stop_reason: Some(stop),
            usage,
        })
    };
    let first = vec![
        ContentBlock::Thinking {
            thinking: "Let me think".into(),
            signature: Some("sig-1".into()),
        },
        ContentBlock::RedactedThinking { data: "abc".into() },
        text("Hi there, done."),
    ];
    let usage = Usage {
        input_tokens: 10,
        output_tokens: 5,
        cache_creation_input_tokens: 3,
        cache_read_input_tokens: 2,
    };
    assert_eq!(
        loaded.messages,
        vec![
            user("msg_ru_u1", "Hello  \"world\"\n\ttab"),
            assistant("msg_rv_a1", first, Some(usage), StopReason::EndTurn),
            user("msg_1jk_u2", "Second"),
            assistant(
                "msg_1jl_a2",
                vec![text("Done")],
                None,
                StopReason::MaxTokens
            ),
        ]
    );
    // 修复线 F20：Node 的 plan 是开关，读回进入 plan 前的权限模式与 plan 开（以前读成单值的 plan）。
    assert_eq!(
        (loaded.mode, loaded.plan),
        (Some(PermissionMode::AcceptEdits), true)
    );
    assert_eq!(loaded.meta.title, "Renamed");
}
