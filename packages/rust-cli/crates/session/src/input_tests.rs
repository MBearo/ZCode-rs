//! 输入能力在会话里的接线（docs/specs/rust-cli/tui-input.md §1.1、§2.2）：普通输入的 `@` 提及在输入之后、UPS
//! 之前产生 attachment；bash 模式记两条人类消息后调模型，被中断时记 caveat、输入与中断消息。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::message::{ContentBlock, Message};
use rust_cli_kernel_api::session_api::SessionHandle;
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use rust_cli_kernel_api::tool::{ToolProgress, ToolUseContext};
use rust_cli_kernel_api::user_input::{BashRun, InputAttachment, UserInputs};
use serde_json::json;
use tokio::sync::mpsc;

use rust_cli_kernel_api::session_api::{Command, Delivery, KernelEvent, TurnOutcome};

use crate::test_support::{Setup, envelope, finished, submit, until};

fn bash(id: &str, line: &str) -> rust_cli_kernel_api::session_api::CommandEnvelope {
    envelope(
        id,
        Command::Bash {
            command: line.to_owned(),
            delivery: Delivery::Queue,
        },
    )
}

/// 每个 `@名字` 给一个 `file` attachment（文字是 `<system-reminder>\nfile 名字\n</system-reminder>`）。
pub(crate) struct FakeInputs;

#[async_trait]
impl UserInputs for FakeInputs {
    async fn mentions(&self, input: &str, _context: ToolUseContext) -> Vec<InputAttachment> {
        input
            .split_whitespace()
            .filter_map(|word| word.strip_prefix('@'))
            .map(|name| InputAttachment {
                source: "file".to_owned(),
                text: format!("<system-reminder>\nfile {name}\n</system-reminder>"),
                data: json!({ "filename": name }),
            })
            .collect()
    }

    async fn bash(
        &self,
        command: &str,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> BashRun {
        match command {
            "slow" => {
                context.cancel.cancelled().await;
                BashRun::Interrupted
            }
            "boom" => BashRun::Failed {
                message: "spawn failed".to_owned(),
            },
            other => BashRun::Completed {
                stdout: format!("ran {other}"),
                stderr: String::new(),
            },
        }
    }
}

fn kinds(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .map(|message| match message {
            Message::User(user) => match &user.content[..] {
                [ContentBlock::Text { text }] => format!("user:{text}"),
                other => format!("user:{other:?}"),
            },
            Message::Attachment(attachment) => format!("attachment:{}", attachment.source),
            Message::Assistant(_) => "assistant".to_owned(),
            Message::System(_) => "system".to_owned(),
        })
        .collect()
}

#[tokio::test]
async fn mentions_are_committed_after_the_input() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["seen"])]));
    let mut setup = Setup::new(model.clone());
    setup.inputs = Arc::new(FakeInputs);
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "look @a.txt @dir")).await;
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    assert_eq!(
        kinds(&view.messages),
        [
            "user:look @a.txt @dir",
            "attachment:file",
            "attachment:file",
            "assistant"
        ]
    );
    let Message::Attachment(first) = &view.messages[1] else {
        panic!("attachment expected");
    };
    assert_eq!(first.data["filename"], "a.txt");
    // 模型收到 attachment 的文字。
    let request = format!("{:?}", model.requests()[0].messages);
    assert!(request.contains("file a.txt") && request.contains("file dir"));
}

#[tokio::test]
async fn plain_input_without_mentions_adds_nothing() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["ok"])]));
    let mut setup = Setup::new(model);
    setup.inputs = Arc::new(FakeInputs);
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "mail me at a@b.c")).await;
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    assert_eq!(
        kinds(&view.messages),
        ["user:mail me at a@b.c", "assistant"]
    );
}

#[tokio::test]
async fn bash_mode_records_input_and_output_then_queries() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["listed"])]));
    let mut setup = Setup::new(model.clone());
    setup.inputs = Arc::new(FakeInputs);
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(bash("c1", "ls")).await;
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    assert_eq!(
        kinds(&view.messages),
        [
            "user:<bash-input>ls</bash-input>",
            "user:<bash-stdout>ran ls</bash-stdout><bash-stderr></bash-stderr>",
            "assistant"
        ]
    );
    let request = format!("{:?}", model.requests()[0].messages);
    assert!(
        request.contains("<bash-stdout>ran ls</bash-stdout>"),
        "{request}"
    );
    // 标题写成 `! 命令`。
    assert_eq!(view.meta.title, "! ls");
}

#[tokio::test]
async fn an_interrupted_bash_command_does_not_query() {
    let model = Arc::new(ScriptedModel::new(vec![]));
    let mut setup = Setup::new(model.clone());
    setup.inputs = Arc::new(FakeInputs);
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(bash("c1", "slow")).await;
    until(&mut events, |event| {
        matches!(event, KernelEvent::TurnStarted { .. })
    })
    .await;
    built.session.send(envelope("i1", Command::Interrupt)).await;
    let seen = until(&mut events, finished).await;
    assert!(seen.iter().any(|event| matches!(
        event,
        KernelEvent::TurnFinished {
            outcome: TurnOutcome::Interrupted,
            ..
        }
    )));
    let view = built.session.view().await.unwrap();
    let texts = kinds(&view.messages);
    assert_eq!(texts.len(), 3, "{texts:?}");
    assert!(
        texts[0]
            .starts_with("user:<local-command-caveat>The command below was run directly in ZCode")
    );
    assert_eq!(texts[1], "user:<bash-input>slow</bash-input>");
    assert_eq!(texts[2], "user:[Request interrupted by user]");
    assert!(model.requests().is_empty());
}

#[tokio::test]
async fn bash_while_running_waits_for_the_turn_to_end() {
    let model = Arc::new(ScriptedModel::new(vec![
        text_response(&["first"]),
        text_response(&["second"]),
    ]));
    let mut setup = Setup::new(model.clone());
    setup.inputs = Arc::new(FakeInputs);
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(bash("c1", "slow")).await;
    until(&mut events, |event| {
        matches!(event, KernelEvent::TurnStarted { .. })
    })
    .await;
    built.session.send(bash("c2", "pwd")).await;
    let queued = until(
        &mut events,
        |event| matches!(event, KernelEvent::QueueChanged { items } if !items.is_empty()),
    )
    .await;
    let Some(KernelEvent::QueueChanged { items }) = queued.last() else {
        panic!("queue expected");
    };
    assert_eq!(items[0].text, "<bash-input>pwd</bash-input>");
    built.session.send(envelope("i1", Command::Interrupt)).await;
    until(&mut events, finished).await;
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    let texts = kinds(&view.messages);
    assert!(
        texts.contains(
            &"user:<bash-stdout>ran pwd</bash-stdout><bash-stderr></bash-stderr>".to_owned()
        ),
        "{texts:?}"
    );
}

#[tokio::test]
async fn submitted_content_keeps_its_images_and_queues_while_running() {
    let image = ContentBlock::Image {
        source: rust_cli_kernel_api::message::ImageSource::Base64 {
            media_type: "image/png".into(),
            data: "iVBORw0KGgo=".into(),
        },
    };
    let content = vec![
        ContentBlock::Text {
            text: "what is [Image #1]".into(),
        },
        image.clone(),
    ];
    let model = Arc::new(ScriptedModel::new(vec![
        text_response(&["first"]),
        text_response(&["a png"]),
    ]));
    let mut setup = Setup::new(model.clone());
    setup.inputs = Arc::new(FakeInputs);
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(bash("c1", "slow")).await;
    until(&mut events, |event| {
        matches!(event, KernelEvent::TurnStarted { .. })
    })
    .await;
    let ack = built
        .session
        .send(envelope(
            "c2",
            Command::SubmitContent {
                content: content.clone(),
                delivery: Delivery::Steer,
            },
        ))
        .await;
    assert!(matches!(
        ack,
        rust_cli_kernel_api::session_api::Ack::Accepted { .. }
    ));
    built.session.send(envelope("i1", Command::Interrupt)).await;
    until(&mut events, finished).await;
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    let user = view
        .messages
        .iter()
        .find_map(|message| match message {
            Message::User(user) if user.content.len() == 2 => Some(user.content.clone()),
            _ => None,
        })
        .expect("the image input");
    assert_eq!(user, content);
    // 带图片的输入不在工具边界并入（D9）：回合结束后单独发，模型收到 image 块。
    let request = format!("{:?}", model.requests().last().unwrap().messages);
    assert!(request.contains("iVBORw0KGgo="), "{request}");
}
