//! 未结调用是谁写的、Node 写的怎么补（docs/specs/rust-cli/node-store.md §4.6、session-lifecycle.md §6.3.2，工作线 F86、
//! F88）：本工程写 pending tool part 时就带 `metadata.rustCli` 标记；没有标记的未结调用是 Node 写的，冷读取在内存里补
//! Node 的中断结果（Node 的文字，按声明顺序夹在已有结果中间），不写库，每次冷读取都在同一位置补；有标记的照旧没有结果
//! （由会话续写时补并落库）。
use rust_cli_kernel_api::ids::{MessageId, SessionId, ToolUseId};
use rust_cli_kernel_api::message::{
    AssistantMessage, CompactBoundary, CompactTrigger, ContentBlock, Message,
    NO_RESPONSE_REQUESTED, PreservedSegment, SYNTHETIC_MODEL, StopReason, SystemMessage,
    ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::store::{Fact, SessionStore};
use serde_json::{Value, json};

use crate::tests::{TestStore, assistant, batch, started, text, user};

/// Node 冷恢复给未结调用补的结果文字（Node `session-history-hydrator.ts` 的 `INTERRUPTED_TOOL_RESULT`）。
const NODE_TEXT: &str = "[Tool execution was interrupted before resume]";

fn sid() -> SessionId {
    SessionId::new("sess_1")
}

fn calls(id: &str, ids: &[&str]) -> Fact {
    Fact::MessageAppended {
        message: Message::Assistant(AssistantMessage {
            id: MessageId::new(id),
            content: std::iter::once(text("Working."))
                .chain(ids.iter().map(|call| ContentBlock::ToolUse {
                    id: ToolUseId::new(*call),
                    name: "Read".into(),
                    input: json!({"file_path": format!("/w/{call}")}),
                }))
                .collect(),
            model: "m".into(),
            stop_reason: Some(StopReason::ToolUse),
            usage: None,
        }),
    }
}

fn read(call: &str) -> Vec<ToolResultContent> {
    vec![ToolResultContent::Text {
        text: format!("read {call}"),
    }]
}

fn result(id: &str, owner: &str, call: &str) -> Fact {
    Fact::MessageAppended {
        message: Message::User(UserMessage {
            id: MessageId::new(id),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: ToolUseId::new(call),
                content: read(call),
                is_error: false,
            }],
            origin: UserOrigin::ToolResult {
                assistant: MessageId::new(owner),
            },
        }),
    }
}

fn sentinel(id: &str) -> Fact {
    Fact::MessageAppended {
        message: Message::Assistant(AssistantMessage {
            id: MessageId::new(id),
            content: vec![text(NO_RESPONSE_REQUESTED)],
            model: SYNTHETIC_MODEL.into(),
            stop_reason: Some(StopReason::StopSequence),
            usage: None,
        }),
    }
}

async fn commit_each(t: &TestStore, facts: Vec<Fact>) {
    for fact in facts {
        t.store
            .commit(&sid(), batch(None, vec![fact]))
            .await
            .unwrap();
    }
}

fn execute(t: &TestStore, sql: &str) {
    t.connection().execute_batch(sql).unwrap();
}

/// 这个调用的 tool part 的 id。
fn part_id(t: &TestStore, call: &str) -> String {
    let rows = t.rows(&format!(
        "select id from part where json_extract(data, '$.type') = 'tool'
           and json_extract(data, '$.callID') = '{call}'"
    ));
    rows[0][0].as_str().unwrap().to_owned()
}

/// 这个调用的 tool part 的 `metadata`（JSON 文本，键序同库里）。
fn part_metadata(t: &TestStore, call: &str) -> Value {
    t.rows(&format!(
        "select json_extract(data, '$.metadata') from part where json_extract(data, '$.type') = 'tool'
           and json_extract(data, '$.callID') = '{call}'"
    ))[0][0]
        .clone()
}

/// 回复 `msg_a1` 的 tool part，按 callID。
fn tool_parts(t: &TestStore) -> Vec<Value> {
    t.rows(
        "select data from part where message_id = 'msg_a1' and json_extract(data, '$.type') = 'tool'
         order by json_extract(data, '$.callID')",
    )
    .into_iter()
    .map(|row| serde_json::from_str(row[0].as_str().unwrap()).unwrap())
    .collect()
}

/// Node 被杀时留下的一步：回复 `msg_a1` 里三个调用，t1 是 Node 开工时写的 running，t2 已由 Node 结算（completed；只读
/// 工具可能先落盘，所以排在没结的 t1 之后），t3 还是 pending；Node 在工具都结算之后才写 `step-finish`，所以没有。先用
/// 本工程的写法写出同样的消息与 part，再改成 Node 的样子：Node 的 tool part 没有 `metadata.rustCli`。
async fn node_killed(t: &TestStore) {
    started(t, "sess_1").await;
    t.source.at(2000);
    commit_each(
        t,
        vec![
            user("msg_u1", "Read three"),
            calls("msg_a1", &["t1", "t2", "t3"]),
            result("msg_r2", "msg_a1", "t2"),
        ],
    )
    .await;
    execute(
        t,
        "delete from part where message_id = 'msg_a1' and json_extract(data, '$.type') = 'step-finish';
         update part set data = json_remove(data, '$.metadata.rustCli')
           where message_id = 'msg_a1' and json_extract(data, '$.type') = 'tool';
         update part set data = json_set(data, '$.state', json('{\"status\":\"running\",\"input\":{\"file_path\":\"/w/t1\"},\"title\":\"Read\",\"metadata\":{},\"time\":{\"start\":2000}}'))
           where json_extract(data, '$.callID') = 't1';",
    );
}

/// Node 的中断结果（`is_error`，Node 的文字，消息 id 是 part id）。
fn node_interrupted(id: &str, call: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new(call),
            content: vec![ToolResultContent::Text {
                text: NODE_TEXT.into(),
            }],
            is_error: true,
        }],
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new("msg_a1"),
        },
    })
}

fn ids(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .map(|message| message.id().as_str().to_owned())
        .collect()
}

/// 本工程写 pending part 时就带标记 `metadata.rustCli = {pending: true}`（空名字时排在 `providerToolName` 之后）；
/// 结算时整个 `rustCli` 换成结果的标记。以前 pending part 的 `metadata` 是 `{}`，与 Node 写的形状相同。
#[tokio::test]
async fn our_pending_parts_carry_the_mark() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let unnamed = Fact::MessageAppended {
        message: Message::Assistant(AssistantMessage {
            id: MessageId::new("msg_a1"),
            content: vec![
                ContentBlock::ToolUse {
                    id: ToolUseId::new("t1"),
                    name: "Read".into(),
                    input: json!({}),
                },
                ContentBlock::ToolUse {
                    id: ToolUseId::new("t2"),
                    name: " ".into(),
                    input: json!({}),
                },
            ],
            model: "m".into(),
            stop_reason: Some(StopReason::ToolUse),
            usage: None,
        }),
    };
    commit_each(&t, vec![user("msg_u1", "Go"), unnamed]).await;
    assert_eq!(part_metadata(&t, "t1"), r#"{"rustCli":{"pending":true}}"#);
    assert_eq!(
        part_metadata(&t, "t2"),
        r#"{"providerToolName":" ","rustCli":{"pending":true}}"#
    );
    commit_each(&t, vec![result("msg_r1", "msg_a1", "t1")]).await;
    assert_eq!(
        part_metadata(&t, "t1"),
        r#"{"rustCli":{"resultMessageId":"msg_r1","resultIndex":0,"afterMessageId":"msg_a1"}}"#
    );
}

/// Node 写的（没有标记的）running 与 pending 调用：冷读取在内存里各补一条 Node 的中断结果，消息 id 是 part id，按
/// 声明顺序夹在 Node 已结算的结果中间（同 Node 冷恢复）。以前补本工程的文字、排在已有结果之后。
#[tokio::test]
async fn node_written_open_calls_get_nodes_interrupted_result_in_place() {
    let t = TestStore::new();
    node_killed(&t).await;
    let before = tool_parts(&t);
    let loaded = t.store.load(&sid()).await.unwrap();
    let (p1, p2, p3) = (part_id(&t, "t1"), part_id(&t, "t2"), part_id(&t, "t3"));
    assert_eq!(
        ids(&loaded.messages),
        [
            "msg_u1".to_owned(),
            "msg_a1".to_owned(),
            p1.clone(),
            p2.clone(),
            p3.clone()
        ]
    );
    assert_eq!(loaded.messages[2], node_interrupted(&p1, "t1"));
    assert_eq!(
        loaded.messages[3],
        Message::User(UserMessage {
            id: MessageId::new(p2),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: ToolUseId::new("t2"),
                content: read("t2"),
                is_error: false,
            }],
            origin: UserOrigin::ToolResult {
                assistant: MessageId::new("msg_a1"),
            },
        })
    );
    assert_eq!(loaded.messages[4], node_interrupted(&p3, "t3"));
    assert_eq!(tool_parts(&t), before, "reading writes nothing");
}

/// 续写之后（占位、新输入、回答都落库）Node 的 tool part 原样不动，也没有操作结果；再冷读取，补的结果还在同一位置，
/// 占位与新输入排在它们之后。
#[tokio::test]
async fn later_commits_leave_the_node_records_as_they_were() {
    let t = TestStore::new();
    node_killed(&t).await;
    let before = tool_parts(&t);
    let first = t.store.load(&sid()).await.unwrap();
    commit_each(&t, vec![sentinel("msg_s1"), user("msg_u2", "Again")]).await;
    commit_each(&t, vec![assistant("msg_a2", vec![text("Done.")], None)]).await;
    assert_eq!(tool_parts(&t), before);
    let statuses: Vec<&str> = before
        .iter()
        .map(|part| part["state"]["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["running", "completed", "pending"]);
    assert!(
        t.rows("select id from session_entry where type = 'rust-cli/operation_result'")
            .is_empty()
    );
    let again = t.store.load(&sid()).await.unwrap();
    assert_eq!(again.messages[..5], first.messages[..]);
    assert_eq!(ids(&again.messages[5..]), ["msg_s1", "msg_u2", "msg_a2"]);
}

/// 本工程写的未结调用（有标记）：冷读取不补，留给会话续写时补并落库（session-lifecycle.md §6.3）。
#[tokio::test]
async fn our_own_open_calls_are_left_to_the_session() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    t.source.at(2000);
    commit_each(
        &t,
        vec![
            user("msg_u1", "Read two"),
            calls("msg_a1", &["t1", "t2"]),
            result("msg_r1", "msg_a1", "t1"),
        ],
    )
    .await;
    let loaded = t.store.load(&sid()).await.unwrap();
    assert_eq!(ids(&loaded.messages), ["msg_u1", "msg_a1", "msg_r1"]);
}

/// 只看标记，不看 `step-finish`：有标记的未结调用在没有 `step-finish` 的回复里也是本工程写的，不补。以前按“回复没有
/// `step-finish`”当成 Node 写的，在内存里补了结果。
#[tokio::test]
async fn a_marked_call_is_ours_even_without_a_step_finish() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    t.source.at(2000);
    commit_each(
        &t,
        vec![user("msg_u1", "Read one"), calls("msg_a1", &["t1"])],
    )
    .await;
    execute(
        &t,
        "delete from part where message_id = 'msg_a1' and json_extract(data, '$.type') = 'step-finish';",
    );
    let loaded = t.store.load(&sid()).await.unwrap();
    assert_eq!(ids(&loaded.messages), ["msg_u1", "msg_a1"]);
}

/// 没有标记的 pending 调用在有 `step-finish` 的回复里也是 Node 写的：工作线 F88 之前本工程写的被杀会话（旧数据），与
/// Node 在工具结果要求停止回合时留下的 pending 都是这个样子。按 Node 写的补：Node 的文字、按声明顺序（本工程写的已结
/// 结果按提交顺序在前）、只在内存，之后的提交不改 part。以前按“回复有 `step-finish`”当成本工程写的，不补，由会话补并落库。
#[tokio::test]
async fn old_sessions_without_the_mark_read_as_node_written() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    t.source.at(2000);
    commit_each(
        &t,
        vec![
            user("msg_u1", "Read two"),
            calls("msg_a1", &["t1", "t2"]),
            result("msg_r1", "msg_a1", "t1"),
        ],
    )
    .await;
    execute(
        &t,
        "update part set data = json_remove(data, '$.metadata.rustCli')
           where json_extract(data, '$.callID') = 't2';",
    );
    let before = tool_parts(&t);
    let loaded = t.store.load(&sid()).await.unwrap();
    let p2 = part_id(&t, "t2");
    assert_eq!(
        ids(&loaded.messages),
        [
            "msg_u1".to_owned(),
            "msg_a1".to_owned(),
            "msg_r1".to_owned(),
            p2.clone()
        ]
    );
    assert_eq!(loaded.messages[3], node_interrupted(&p2, "t2"));
    commit_each(&t, vec![sentinel("msg_s1"), user("msg_u2", "Again")]).await;
    assert_eq!(tool_parts(&t), before);
    let again = t.store.load(&sid()).await.unwrap();
    assert_eq!(again.messages[..4], loaded.messages[..]);
}

/// 压缩的保留段以补在内存里的结果结尾：存储按 part id 换成它的回复那一行（compact.md §6）。以前找不到这一行，
/// 提交报 `preserved message … has no stored row`。
#[tokio::test]
async fn a_preserved_segment_can_end_on_an_in_memory_result() {
    let t = TestStore::new();
    node_killed(&t).await;
    let tail = part_id(&t, "t3");
    let boundary = Message::System(SystemMessage::CompactBoundary(CompactBoundary {
        id: MessageId::new("msg_b"),
        trigger: CompactTrigger::Manual,
        pre_tokens: 100,
        summary: MessageId::new("msg_s"),
        preserved: Some(PreservedSegment {
            head: MessageId::new("msg_a1"),
            anchor: MessageId::new("msg_s"),
            tail: MessageId::new(tail.as_str()),
        }),
        discovered_tools: Vec::new(),
    }));
    let summary = Message::User(UserMessage {
        id: MessageId::new("msg_s"),
        content: vec![text("This session is being continued…")],
        origin: UserOrigin::CompactSummary,
    });
    let facts = vec![
        Fact::MessageAppended { message: boundary },
        Fact::MessageAppended { message: summary },
    ];
    t.store.commit(&sid(), batch(None, facts)).await.unwrap();
    let rows = t.rows(
        "select json_extract(data, '$.compactBoundary.preservedSegment.tailMessageId')
         from part where message_id = 'msg_s' and json_extract(data, '$.type') = 'compaction'",
    );
    assert_eq!(rows[0][0], "msg_a1");
}
