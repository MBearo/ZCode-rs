//! 本地命令的记录（修复线 L-A，docs/specs/rust-cli/tui-manage.md §11）：空闲时一批提交，忙时留到收尾；不是人类输入；
//! 下一个请求带上。

use std::sync::Arc;

use rust_cli_kernel_api::message::{ContentBlock, Message, UserOrigin};
use rust_cli_kernel_api::model::{ApiContent, ApiRole};
use rust_cli_kernel_api::session_api::{Ack, Command, KernelEvent, SessionHandle};
use rust_cli_kernel_api::store::Fact;
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use rust_cli_kernel_api::user_command::{
    LOCAL_COMMAND_CAVEAT_SOURCE, LOCAL_COMMAND_INPUT_SOURCE, LOCAL_COMMAND_META_SOURCE,
    LOCAL_COMMAND_OUTPUT_SOURCE, LocalCommandOutput, LocalCommandRecord,
};

use crate::test_support::{Setup, envelope, finished, submit, until};

fn record(name: &str, args: &str, stdout: &str, caveat: bool) -> LocalCommandRecord {
    LocalCommandRecord {
        name: name.to_owned(),
        args: args.to_owned(),
        output: LocalCommandOutput::Stdout(stdout.to_owned()),
        caveat,
        meta: Vec::new(),
    }
}

fn sources(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user) => match &user.origin {
                UserOrigin::Meta { source } => Some(source.clone()),
                UserOrigin::Human => Some("human".to_owned()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// 请求里最后一条 user 消息的文字块。
fn last_user_texts(request: &rust_cli_kernel_api::model::ModelRequest) -> Vec<String> {
    let user = request
        .messages
        .iter()
        .rev()
        .find(|message| message.role == ApiRole::User)
        .expect("a user message");
    user.content
        .iter()
        .filter_map(|ApiContent { block, .. }| match block {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn an_idle_record_is_one_batch_and_the_next_request_carries_it() {
    let model = Arc::new(ScriptedModel::new(vec![
        text_response(&["first"]),
        text_response(&["second"]),
    ]));
    let built = Setup::new(model.clone()).build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "hello")).await;
    until(&mut events, finished).await;
    let mut effort = record(
        "effort",
        "high",
        "Set effort level to high (saved as your default for new sessions): x",
        true,
    );
    effort.meta = vec!["a meta line".to_owned()];
    let before = built.store.facts(&crate::test_support::meta().id).len();
    let ack = built
        .session
        .send(envelope(
            "c2",
            Command::RecordLocalCommand {
                record: effort.clone(),
            },
        ))
        .await;
    assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
    // 一批四条 meta 消息，顺序是固定说明、命令输入、输出、meta。
    let facts = built.store.facts(&crate::test_support::meta().id);
    let added: Vec<Message> = facts[before..]
        .iter()
        .filter_map(|fact| match fact {
            Fact::MessageAppended { message } => Some(message.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        sources(&added),
        [
            LOCAL_COMMAND_CAVEAT_SOURCE,
            LOCAL_COMMAND_INPUT_SOURCE,
            LOCAL_COMMAND_OUTPUT_SOURCE,
            LOCAL_COMMAND_META_SOURCE
        ]
    );
    // 不是人类输入：当前轮不变（批次的轮次与上一条输入的相同）。
    let turns = built.store.batch_turns(&crate::test_support::meta().id);
    assert_eq!(turns.last(), turns.get(turns.len() - 2));
    built.session.send(submit("c3", "next")).await;
    until(&mut events, finished).await;
    let requests = model.requests();
    let texts = last_user_texts(&requests[1]);
    let expected: Vec<String> = effort
        .texts()
        .into_iter()
        .map(|(_, text)| text)
        .chain(["next".to_owned()])
        .collect();
    // 相邻 user 消息合并时前一段末尾补一个换行（context.md §3）。
    let merged: Vec<String> = expected
        .iter()
        .enumerate()
        .map(|(index, text)| {
            if index + 1 < expected.len() {
                format!("{text}\n")
            } else {
                text.clone()
            }
        })
        .collect();
    assert!(texts.ends_with(&merged), "{texts:#?}");
}

#[tokio::test]
async fn a_record_during_a_turn_waits_until_the_turn_ends() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let model = Arc::new(crate::test_support::GatedModel::new(gate.clone()));
    let built = Setup::new(model).build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "hello")).await;
    until(&mut events, |event| {
        matches!(event, KernelEvent::TurnStarted { .. })
    })
    .await;
    let ack = built
        .session
        .send(envelope(
            "c2",
            Command::RecordLocalCommand {
                record: record("model", "x", "Set model to `x`", true),
            },
        ))
        .await;
    assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
    // 回合还在跑：记录不在历史里。
    let view = built.session.view().await.unwrap();
    assert!(
        !sources(&view.messages).contains(&LOCAL_COMMAND_INPUT_SOURCE.to_owned()),
        "{:?}",
        sources(&view.messages)
    );
    gate.notify_one();
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    // 收尾后排在这一回合的消息（assistant）之后。
    let at = view
        .messages
        .iter()
        .position(|message| {
            matches!(message, Message::User(user) if user.origin == UserOrigin::Meta { source: LOCAL_COMMAND_INPUT_SOURCE.to_owned() })
        })
        .expect("the record after the turn");
    assert!(
        view.messages[..at]
            .iter()
            .any(|message| matches!(message, Message::Assistant(_))),
        "{:?}",
        view.messages
    );
}

/// `/clear` 之后的新会话以记录开头：第一条输入仍是首个回合（起标题），记录不算 user 消息。
#[tokio::test]
async fn records_before_the_first_input_keep_it_the_first_turn() {
    let main = Arc::new(ScriptedModel::new(vec![text_response(&["hi"])]));
    let title_model = Arc::new(ScriptedModel::new(vec![text_response(&[
        "{\"title\":\"Fresh start\"}",
    ])]));
    let mut setup = Setup::new(main);
    setup.titles = Some(crate::TitleModel {
        model: title_model.clone(),
        directory: None,
    });
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built
        .session
        .send(envelope(
            "c1",
            Command::RecordLocalCommand {
                record: record("clear", "", "", true),
            },
        ))
        .await;
    built
        .session
        .send(submit("c2", "Start over please now"))
        .await;
    until(
        &mut events,
        |event| matches!(event, KernelEvent::TitleChanged { title } if title == "Fresh start"),
    )
    .await;
    assert_eq!(title_model.requests().len(), 1);
}
