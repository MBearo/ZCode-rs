//! `/fork` 按用户输入分叉（session-lifecycle.md §6.5）：复制到所选那一轮的最后一条回答、文件不动、拒绝的情形。
use std::sync::Arc;

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rust_cli_kernel_api::ids::{MessageId, OwnerEpoch, SessionId, ToolUseId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::model::{ModelError, ModelEvent, ModelPort, ModelRequest};
use rust_cli_kernel_api::session_api::{Ack, Command, CommandEnvelope, SessionHandle};
use rust_cli_kernel_api::store::{Fact, FactBatch, SessionStore};
use rust_cli_kernel_api::testing::{MemoryStore, ScriptedModel, text_response, tool_use_response};
use rust_cli_kernel_api::user_input::INTERRUPT_TEXTS;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::through;
use crate::Opened;
use crate::rewind::tests::{Writing, file};
use crate::test_support::{Setup, envelope, finished, meta, submit, until};

/// 命令 ID 取输入的 ID：同一个 ID 再发是重复的命令（`Duplicate`）。
fn fork(input: &MessageId) -> CommandEnvelope {
    envelope(
        &format!("fork-{input}"),
        Command::ForkFromInput {
            input: input.clone(),
        },
    )
}

fn user(id: &str, text: &str, origin: UserOrigin) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        origin,
    })
}

fn human(id: &str, text: &str) -> Message {
    user(id, text, UserOrigin::Human)
}

fn assistant(id: &str, content: ContentBlock) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new(id),
        content: vec![content],
        model: "m".to_owned(),
        stop_reason: None,
        usage: None,
    })
}

fn said(text: &str) -> ContentBlock {
    ContentBlock::Text {
        text: text.to_owned(),
    }
}

fn texts(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| {
            let content = match message {
                Message::User(user) => &user.content,
                Message::Assistant(assistant) => &assistant.content,
                _ => return None,
            };
            content.iter().find_map(|block| match block {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
        })
        .collect()
}

/// 三轮对话（第二轮写了文件、有工具轮）：从第二条输入分叉，新会话正好到第二轮的回答，文件不动。
#[tokio::test]
async fn forking_at_the_second_input_keeps_that_turn_and_its_answer() {
    let model = Arc::new(ScriptedModel::new(vec![
        text_response(&["one"]),
        tool_use_response(
            "toolu_1",
            "Write",
            &json!({"file_path": "/work/a.txt", "content": "new\n"}),
        ),
        text_response(&["two"]),
        text_response(&["three"]),
    ]));
    let env = crate::test_support::env();
    env.set_file("/work/a.txt", "old\n");
    let mut setup = Setup::new(model);
    setup.set_env(env.clone());
    setup.tools = vec![Arc::new(Writing)];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    for (id, text) in [("c1", "first"), ("c2", "second"), ("c3", "third")] {
        built.session.send(submit(id, text)).await;
        until(&mut events, finished).await;
    }
    let messages = built.session.view().await.unwrap().messages;
    let id_of = |text: &str| {
        messages
            .iter()
            .find(|message| texts(std::slice::from_ref(*message)) == [text])
            .map(|message| message.id().clone())
            .unwrap()
    };
    let (second, answer) = (id_of("second"), id_of("two"));
    let ack = built.session.send(fork(&second)).await;
    let Ack::Accepted { result } = ack else {
        panic!("{ack:?}")
    };
    let child = SessionId::new(result["session"].as_str().unwrap());
    let end = messages.iter().position(|m| m.id() == &answer).unwrap() + 1;
    assert_eq!(
        result["response"],
        format!("Forked session {child} from message {answer}: copied {end} messages.")
    );
    // 回执带复制到的那条消息（界面的中文提示用），不再带总是空的 `restored`。
    assert_eq!(result["message"], json!(answer.as_str()));
    assert!(result.get("restored").is_none(), "{result}");
    let loaded = built.store.load(&child).await.unwrap();
    assert_eq!(loaded.meta.parent, Some(meta().id));
    // 复制的是前两轮（含第二轮的工具调用、结果与最后的回答），之后是分叉提示；第三轮不在。
    assert_eq!(loaded.messages.len(), end + 1, "{:?}", loaded.messages);
    assert_eq!(loaded.messages[..end], messages[..end]);
    assert_eq!(texts(&loaded.messages), ["first", "one", "second", "two"]);
    assert!(matches!(&loaded.messages[end], Message::Attachment(a) if a.source == "fork"));
    // 文件不动：第二轮写的内容还在。
    assert_eq!(file(&env, "/work/a.txt").as_deref(), Some("new\n"));
    // 原会话不变。
    assert_eq!(built.session.view().await.unwrap().messages, messages);
}

/// 续写的会话以没有回答的输入结束：内存里补了 `No response requested.`，它不算回答，新会话复制到那条输入
/// （拿它当终点时存储找不到这条消息，分叉失败）。
#[tokio::test]
async fn a_turn_without_an_answer_is_copied_up_to_its_input() {
    let store = Arc::new(MemoryStore::default());
    store.create(meta()).await.unwrap();
    let history = vec![
        human("msg_u1", "first"),
        assistant("msg_a1", said("one")),
        human("msg_u2", "second"),
    ];
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
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(Vec::new())));
    setup.store = Some(store.clone());
    setup.messages = history.clone();
    setup.opened = Opened::Loaded;
    let built = setup.build().await;
    let view = built.session.view().await.unwrap().messages;
    assert!(
        matches!(view.last(), Some(Message::Assistant(a)) if a.is_resume_sentinel()),
        "{view:?}"
    );
    let ack = built.session.send(fork(&MessageId::new("msg_u2"))).await;
    let Ack::Accepted { result } = ack else {
        panic!("{ack:?}")
    };
    let child = SessionId::new(result["session"].as_str().unwrap());
    assert_eq!(
        result["response"],
        format!("Forked session {child} from message msg_u2: copied 3 messages.")
    );
    let loaded = store.load(&child).await.unwrap();
    assert_eq!(loaded.messages[..3], history[..]);
    assert_eq!(loaded.messages.len(), 4);
}

/// 一直不回复的模型：回合开始后一直在跑。
struct Silent;

impl ModelPort for Silent {
    fn stream(
        &self,
        _request: ModelRequest,
        _cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
        futures_util::stream::pending().boxed()
    }
}

/// 只能从可选的输入分叉；回合在跑时拒绝。
#[tokio::test]
async fn only_an_input_can_be_forked_and_not_while_a_turn_runs() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["one"])]));
    let built = Setup::new(model).build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "first")).await;
    until(&mut events, finished).await;
    let messages = built.session.view().await.unwrap().messages;
    let answer = messages
        .iter()
        .find(|message| matches!(message, Message::Assistant(_)))
        .unwrap()
        .id()
        .clone();
    for target in [answer, MessageId::new("msg_nope")] {
        let ack = built.session.send(fork(&target)).await;
        assert_eq!(
            ack,
            Ack::Rejected {
                reason: format!("{target} is not a message to fork from")
            }
        );
    }
    let running = Setup::new(Arc::new(Silent)).build().await;
    running.session.send(submit("c1", "hello")).await;
    let input = running.session.view().await.unwrap().messages[0]
        .id()
        .clone();
    let ack = running.session.send(fork(&input)).await;
    assert_eq!(
        ack,
        Ack::Rejected {
            reason: "a turn is running; fork after it finishes".to_owned()
        }
    );
}

/// 这一轮到下一条可选的输入为止：中断标记、工具结果、后台通知与它的回答都在这一轮里，终点是其中最后一条 assistant。
#[test]
fn the_turn_runs_until_the_next_input() {
    let result = Message::User(UserMessage {
        id: MessageId::new("r2"),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new("t"),
            content: vec![ToolResultContent::Text {
                text: "ok".to_owned(),
            }],
            is_error: false,
        }],
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new("a2"),
        },
    });
    let notification = user(
        "n",
        "<task-notification>done</task-notification>",
        UserOrigin::Meta {
            source: "task-notification".to_owned(),
        },
    );
    let call = ContentBlock::ToolUse {
        id: ToolUseId::new("t"),
        name: "Bash".to_owned(),
        input: json!({}),
    };
    let messages = vec![
        human("u1", "first"),
        assistant("a1", said("one")),
        human("u2", "second"),
        assistant("a2", call),
        result,
        assistant("a3", said("two")),
        human("i2", INTERRUPT_TEXTS[0]),
        notification,
        assistant("a4", said("noted")),
        human("u3", "third"),
        assistant("a5", said("three")),
    ];
    let id = MessageId::new;
    assert_eq!(through(&messages, &id("u1")), Some(id("a1")));
    assert_eq!(through(&messages, &id("u2")), Some(id("a4")));
    assert_eq!(through(&messages, &id("u3")), Some(id("a5")));
    for not_an_input in ["a1", "r2", "i2", "n", "nope"] {
        assert_eq!(
            through(&messages, &id(not_an_input)),
            None,
            "{not_an_input}"
        );
    }
}
