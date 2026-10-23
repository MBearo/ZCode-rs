//! 斜杠命令在回合任务里的展开（ext.md §2.7、session-lifecycle.md §3.1）：先展开、再跑 UPS；不调模型的
//! 本地结果不跑 UPS；展开期间被中断照常提交输入，再写中断标记（修复线 F37）。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::hooks::{HookEventName, HookOutcome};
use rust_cli_kernel_api::message::{ContentBlock, Message, UserOrigin};
use rust_cli_kernel_api::session_api::{Command, KernelEvent, SessionHandle, TurnOutcome};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use rust_cli_kernel_api::tool::ToolUseContext;
use rust_cli_kernel_api::user_command::{
    ExpandedKind, ExpandedMessage, Expansion, UserCommand, UserCommands, command_tags,
};

use crate::test_support::{ScriptedHooks, Setup, envelope, finished, submit, until};

/// 只认 `/greet` 与 `/inner` 的替身；`/slow` 等到回合被中断才返回。
struct FakeCommands;

fn message(text: &str, kind: ExpandedKind) -> ExpandedMessage {
    ExpandedMessage {
        text: text.to_owned(),
        kind,
    }
}

fn meta(source: &str) -> ExpandedKind {
    ExpandedKind::Meta {
        source: source.to_owned(),
    }
}

#[async_trait]
impl UserCommands for FakeCommands {
    async fn commands(&self, _workspace: &WorkspaceRef) -> Vec<UserCommand> {
        Vec::new()
    }

    async fn expand(&self, input: &str, context: ToolUseContext) -> Expansion {
        match input {
            "/greet Ada" => Expansion::Messages {
                messages: vec![
                    message(&command_tags("greet", "Ada"), ExpandedKind::Input),
                    message("Say hello to Ada.", meta("skill")),
                ],
                query: true,
            },
            "/inner" => Expansion::Messages {
                messages: vec![
                    message("caveat", meta("local-command-caveat")),
                    message("/inner", ExpandedKind::Input),
                    message("only the model", meta("local-command")),
                ],
                query: false,
            },
            "/slow" => {
                context.cancel.cancelled().await;
                Expansion::Messages {
                    messages: vec![message("late", ExpandedKind::Input)],
                    query: true,
                }
            }
            _ => Expansion::NotCommand,
        }
    }
}

fn texts(messages: &[Message]) -> Vec<(String, String)> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user) => {
                let origin = match &user.origin {
                    UserOrigin::Human => "human".to_owned(),
                    UserOrigin::Meta { source } => source.clone(),
                    other => format!("{other:?}"),
                };
                let text = match &user.content[..] {
                    [ContentBlock::Text { text }] => text.clone(),
                    other => format!("{other:?}"),
                };
                Some((origin, text))
            }
            _ => None,
        })
        .collect()
}

fn pair(origin: &str, text: &str) -> (String, String) {
    (origin.to_owned(), text.to_owned())
}

#[tokio::test]
async fn a_skill_command_expands_before_the_prompt_hook() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["hi Ada"])]));
    let hooks = Arc::new(ScriptedHooks::with(
        HookEventName::UserPromptSubmit,
        HookOutcome::default(),
    ));
    let mut setup = Setup::new(model.clone());
    setup.hooks = hooks.clone();
    setup.commands = Arc::new(FakeCommands);
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "/greet Ada")).await;
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    let tags = command_tags("greet", "Ada");
    assert_eq!(
        texts(&view.messages),
        [pair("human", &tags), pair("skill", "Say hello to Ada.")]
    );
    // 模型收到标签与正文；UPS 的 prompt 是原文（UPS 在展开之后，看的是输入原文）。
    let request = format!("{:?}", model.requests()[0].messages);
    assert!(request.contains("command-message") && request.contains("Say hello to Ada."));
    assert_eq!(hooks.seen()[0].payload["prompt"], "/greet Ada");
    // 标题的输入还原成用户输入的样子（ext.md §2.7）。
    assert_eq!(view.meta.title, "/greet Ada");
}

#[tokio::test]
async fn a_local_command_commits_without_hooks_or_the_model() {
    let model = Arc::new(ScriptedModel::new(Vec::new()));
    let hooks = Arc::new(ScriptedHooks::with(
        HookEventName::UserPromptSubmit,
        HookOutcome::default(),
    ));
    let mut setup = Setup::new(model.clone());
    setup.hooks = hooks.clone();
    setup.commands = Arc::new(FakeCommands);
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "/inner")).await;
    let seen = until(&mut events, finished).await;
    assert!(matches!(
        seen.last(),
        Some(KernelEvent::TurnFinished {
            outcome: TurnOutcome::Success,
            ..
        })
    ));
    let view = built.session.view().await.unwrap();
    assert_eq!(
        texts(&view.messages),
        [
            pair("local-command-caveat", "caveat"),
            pair("human", "/inner"),
            pair("local-command", "only the model"),
        ]
    );
    assert!(model.requests().is_empty());
    assert!(hooks.seen().is_empty());
}

/// 展开期间被中断（修复线 F37，ext.md 原 D26）：展开出的输入照常提交，主循环写中断标记、不调模型（提交命令输入与
/// 中断消息）。以前什么都不提交。
#[tokio::test]
async fn an_interrupt_during_the_expansion_keeps_the_input_and_marks_it() {
    let model = Arc::new(ScriptedModel::new(Vec::new()));
    let mut setup = Setup::new(model.clone());
    setup.commands = Arc::new(FakeCommands);
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "/slow")).await;
    until(&mut events, |event| {
        matches!(event, KernelEvent::TurnStarted { .. })
    })
    .await;
    built.session.send(envelope("c2", Command::Interrupt)).await;
    let seen = until(&mut events, finished).await;
    assert!(matches!(
        seen.last(),
        Some(KernelEvent::TurnFinished {
            outcome: TurnOutcome::Interrupted,
            ..
        })
    ));
    assert_eq!(
        texts(&built.session.view().await.unwrap().messages),
        [
            pair("human", "late"),
            pair("human", "[Request interrupted by user]")
        ]
    );
    assert!(model.requests().is_empty());
}

#[tokio::test]
async fn unknown_commands_go_to_the_model_as_typed() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["ok"])]));
    let mut setup = Setup::new(model.clone());
    setup.commands = Arc::new(FakeCommands);
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "/nope foo")).await;
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    assert_eq!(texts(&view.messages), [pair("human", "/nope foo")]);
    assert_eq!(view.meta.title, "/nope foo");
}
