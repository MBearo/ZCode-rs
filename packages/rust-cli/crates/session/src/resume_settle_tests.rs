//! 进程被杀后续写（docs/specs/rust-cli/session-lifecycle.md §6.3，工作线 F81 R3）：末尾没有结果的调用补一条中断结果，
//! 排在已有结果之后，随第一批带消息的提交落库；更早的、没有结果的 tool_use 只去掉这些块，文字留着。
use std::sync::Arc;

use rust_cli_kernel_api::ids::{MessageId, OwnerEpoch, SessionId, ToolUseId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, StopReason, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::model::{ApiMessage, ApiRole};
use rust_cli_kernel_api::session_api::{Ack, Command, SessionHandle};
use rust_cli_kernel_api::store::{Fact, FactBatch, SessionStore};
use rust_cli_kernel_api::testing::{MemoryStore, ScriptedModel, text_response};
use serde_json::json;

use crate::Opened;
use crate::test_support::{Built, Setup, envelope, finished, meta, submit, until};

const INTERRUPTED: &str = "[Tool call interrupted: the session ended before this call's result was recorded, so its outcome is unknown. Check whether it took effect before relying on it or running it again.]";

fn text(value: &str) -> ContentBlock {
    ContentBlock::Text {
        text: value.to_owned(),
    }
}

fn human(id: &str, value: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![text(value)],
        origin: UserOrigin::Human,
    })
}

fn call(id: &str) -> ContentBlock {
    ContentBlock::ToolUse {
        id: ToolUseId::new(id),
        name: "Bash".to_owned(),
        input: json!({}),
    }
}

fn assistant(id: &str, content: Vec<ContentBlock>) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new(id),
        content,
        model: "m".to_owned(),
        stop_reason: Some(StopReason::ToolUse),
        usage: None,
    })
}

fn result(id: &str, owner: &str, call: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new(call),
            content: vec![ToolResultContent::Text {
                text: "done".to_owned(),
            }],
            is_error: false,
        }],
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new(owner),
        },
    })
}

/// 存储里有这段历史，会话从存储打开；模型按序回两段文字。
async fn resumed(history: Vec<Message>) -> (Built, Arc<ScriptedModel>) {
    let store = Arc::new(MemoryStore::default());
    store.create(meta()).await.unwrap();
    let batch = FactBatch {
        owner_epoch: OwnerEpoch::LOCAL,
        command: None,
        facts: history
            .iter()
            .map(|message| Fact::MessageAppended {
                message: message.clone(),
            })
            .collect(),
        turn: None,
    };
    store.commit(&meta().id, batch).await.unwrap();
    let model = Arc::new(ScriptedModel::new(vec![
        text_response(&["second answer"]),
        text_response(&["third answer"]),
    ]));
    let mut setup = Setup::new(model.clone());
    setup.store = Some(store);
    setup.messages = history;
    setup.opened = Opened::Loaded;
    (setup.build().await, model)
}

/// 请求里每条消息的简写：角色与块（文字、`use:<id>`、`res:<id>:<文字>[:err]`）。
fn shape(messages: &[ApiMessage]) -> Vec<(ApiRole, Vec<String>)> {
    messages
        .iter()
        .map(|message| {
            let blocks = message
                .content
                .iter()
                .map(|item| match &item.block {
                    ContentBlock::Text { text } => text.clone(),
                    ContentBlock::ToolUse { id, .. } => format!("use:{id}"),
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        let body = match content.as_slice() {
                            [ToolResultContent::Text { text }] => text.as_str(),
                            _ => "?",
                        };
                        format!(
                            "res:{tool_use_id}:{body}{}",
                            if *is_error { ":err" } else { "" }
                        )
                    }
                    other => format!("{other:?}"),
                })
                .collect();
            (message.role, blocks)
        })
        .collect()
}

fn s(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

/// 存储里的消息：续写占位记作 `S`，补的中断结果记作 `interrupted <调用>`，其余是消息 ID。
fn stored(store: &MemoryStore) -> Vec<String> {
    store
        .facts(&SessionId::new("sess_1"))
        .iter()
        .filter_map(|fact| match fact {
            Fact::MessageAppended {
                message: Message::Assistant(a),
            } if a.is_resume_sentinel() => Some("S".to_owned()),
            Fact::MessageAppended {
                message: Message::User(user),
            } => match user.content.first() {
                Some(ContentBlock::ToolResult {
                    tool_use_id,
                    is_error: true,
                    ..
                }) => Some(format!("interrupted {tool_use_id}")),
                _ => Some(user.id.as_str().to_owned()),
            },
            Fact::MessageAppended { message } => Some(message.id().as_str().to_owned()),
            _ => None,
        })
        .collect()
}

async fn continue_with(built: &Built, command: &str, input: &str) {
    let mut events = built.session.subscribe(None);
    built.session.send(submit(command, input)).await;
    until(&mut events, finished).await;
}

/// t1 的结果已落库、t2 还在执行时进程被杀：续写时 t2 补一条中断结果，排在 t1 的结果之后，再补占位。以前留着
/// 没有结果的 t2，请求里的 tool_use 不配对。
#[tokio::test]
async fn a_call_left_without_a_result_gets_an_interrupted_result_after_the_others() {
    let history = vec![
        human("msg_u1", "Go"),
        assistant("msg_a1", vec![text("Reading."), call("t1"), call("t2")]),
        result("msg_r1", "msg_a1", "t1"),
    ];
    let (built, model) = resumed(history).await;
    continue_with(&built, "c1", "Again.").await;
    let request = &model.requests()[0];
    assert_eq!(
        shape(&request.messages),
        vec![
            (ApiRole::User, s(&["Go"])),
            (ApiRole::Assistant, s(&["Reading.", "use:t1", "use:t2"])),
            (
                ApiRole::User,
                vec![
                    "res:t1:done".to_owned(),
                    format!("res:t2:{INTERRUPTED}:err")
                ]
            ),
            (ApiRole::Assistant, s(&["No response requested."])),
            (ApiRole::User, s(&["Again."])),
        ]
    );
}

/// 补的中断结果随第一批带消息的提交落库，排在占位之前；之后的回合不再补。
#[tokio::test]
async fn the_interrupted_result_is_stored_once_before_the_placeholder() {
    let history = vec![
        human("msg_u1", "Go"),
        assistant("msg_a1", vec![call("t1"), call("t2")]),
        result("msg_r1", "msg_a1", "t1"),
    ];
    let (built, model) = resumed(history).await;
    continue_with(&built, "c1", "Again.").await;
    continue_with(&built, "c2", "Once more.").await;
    let stored = stored(&built.store);
    assert_eq!(
        stored[..5],
        s(&["msg_u1", "msg_a1", "msg_r1", "interrupted t2", "S"])
    );
    assert_eq!(
        stored
            .iter()
            .filter(|id| id.starts_with("interrupted"))
            .count(),
        1,
        "{stored:?}"
    );
    // 第二次请求里它照样在原处。
    let second = shape(&model.requests()[1].messages);
    assert_eq!(
        second[2],
        (
            ApiRole::User,
            vec![
                "res:t1:done".to_owned(),
                format!("res:t2:{INTERRUPTED}:err")
            ]
        )
    );
}

/// 回复的调用全都没有结果：回复的文字与调用都留着，每个调用补一条中断结果。以前整条回复去掉，文字一起丢。
#[tokio::test]
async fn a_reply_whose_calls_all_lack_results_keeps_its_text() {
    let history = vec![
        human("msg_u1", "Go"),
        assistant("msg_a1", vec![text("Reading."), call("t1")]),
    ];
    let (built, model) = resumed(history).await;
    continue_with(&built, "c1", "Again.").await;
    assert_eq!(
        shape(&model.requests()[0].messages),
        vec![
            (ApiRole::User, s(&["Go"])),
            (ApiRole::Assistant, s(&["Reading.", "use:t1"])),
            (ApiRole::User, vec![format!("res:t1:{INTERRUPTED}:err")]),
            (ApiRole::Assistant, s(&["No response requested."])),
            (ApiRole::User, s(&["Again."])),
        ]
    );
}

/// 更早的回复里没有结果的调用（之后已有新的输入）：只去掉这个 tool_use 块，文字留着，不补结果。以前整条回复去掉。
#[tokio::test]
async fn an_earlier_call_without_a_result_loses_only_its_block() {
    let history = vec![
        human("msg_u1", "Go"),
        assistant("msg_a1", vec![text("Looking."), call("t1")]),
        human("msg_u2", "Never mind."),
        Message::Assistant(AssistantMessage {
            id: MessageId::new("msg_a2"),
            content: vec![text("OK.")],
            model: "m".to_owned(),
            stop_reason: Some(StopReason::EndTurn),
            usage: None,
        }),
    ];
    let (built, model) = resumed(history).await;
    continue_with(&built, "c1", "Next.").await;
    assert_eq!(
        shape(&model.requests()[0].messages),
        vec![
            (ApiRole::User, s(&["Go"])),
            (ApiRole::Assistant, s(&["Looking."])),
            (ApiRole::User, s(&["Never mind."])),
            (ApiRole::Assistant, s(&["OK."])),
            (ApiRole::User, s(&["Next."])),
        ]
    );
}

/// 会话里切过去时（`Command::StoreResumeSentinel`）补的中断结果与占位一起单独写进存储，按历史里的顺序；之后的输入
/// 不再带它们。
#[tokio::test]
async fn the_store_command_writes_the_interrupted_result_with_the_placeholder() {
    let history = vec![
        human("msg_u1", "Go"),
        assistant("msg_a1", vec![call("t1"), call("t2")]),
        result("msg_r1", "msg_a1", "t1"),
    ];
    let (built, _model) = resumed(history).await;
    let ack = built
        .session
        .send(envelope("c0", Command::StoreResumeSentinel))
        .await;
    assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
    assert_eq!(
        stored(&built.store),
        s(&["msg_u1", "msg_a1", "msg_r1", "interrupted t2", "S"])
    );
    continue_with(&built, "c1", "Again.").await;
    let after = stored(&built.store);
    assert_eq!(
        after
            .iter()
            .filter(|id| id.starts_with("interrupted"))
            .count(),
        1,
        "{after:?}"
    );
}

/// 存储冷读取时已在内存里补好的结果（Node 写的未结调用，文字是 Node 的，session-lifecycle.md §6.3.2，工作线 F86、F88）：
/// 会话把它当作已有结果，不再补，也不写进存储——切过去时的单独一批与之后的提交都只有占位与新的消息。
#[tokio::test]
async fn a_result_the_store_supplied_in_memory_is_never_stored() {
    const SUPPLIED: &str = "[Tool execution was interrupted before resume]";
    let stored_history = vec![
        human("msg_u1", "Go"),
        assistant("msg_a1", vec![call("t1"), call("t2")]),
        result("msg_r1", "msg_a1", "t1"),
    ];
    let supplied = Message::User(UserMessage {
        id: MessageId::new("part_t2"),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new("t2"),
            content: vec![ToolResultContent::Text {
                text: SUPPLIED.to_owned(),
            }],
            is_error: true,
        }],
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new("msg_a1"),
        },
    });
    let store = Arc::new(MemoryStore::default());
    store.create(meta()).await.unwrap();
    let batch = FactBatch {
        owner_epoch: OwnerEpoch::LOCAL,
        command: None,
        facts: stored_history
            .iter()
            .map(|message| Fact::MessageAppended {
                message: message.clone(),
            })
            .collect(),
        turn: None,
    };
    store.commit(&meta().id, batch).await.unwrap();
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["second answer"])]));
    let mut setup = Setup::new(model.clone());
    setup.store = Some(store);
    setup.messages = [stored_history, vec![supplied]].concat();
    setup.opened = Opened::Loaded;
    let built = setup.build().await;
    let ack = built
        .session
        .send(envelope("c0", Command::StoreResumeSentinel))
        .await;
    assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
    assert_eq!(
        stored(&built.store),
        s(&["msg_u1", "msg_a1", "msg_r1", "S"])
    );
    continue_with(&built, "c1", "Again.").await;
    let after = stored(&built.store);
    assert!(
        !after.iter().any(|id| id.starts_with("interrupted")),
        "{after:?}"
    );
    assert_eq!(
        shape(&model.requests()[0].messages)[2],
        (
            ApiRole::User,
            vec!["res:t1:done".to_owned(), format!("res:t2:{SUPPLIED}:err")]
        )
    );
}
