use std::sync::Arc;

use rust_cli_kernel_api::hooks::{HookEventName, HookOutcome};
use rust_cli_kernel_api::message::{ContentBlock, Message, UserMessage, UserOrigin};
use rust_cli_kernel_api::session_api::{
    Command, EndReason, KernelEvent, SessionHandle, StartSource, TurnOutcome,
};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use tokio::sync::Notify;

use crate::test_support::{ScriptedHooks, Setup, envelope, finished, submit, until};

fn meta_message(id: &str, text: &str) -> Message {
    Message::User(UserMessage {
        id: rust_cli_kernel_api::ids::MessageId::new(id),
        content: vec![ContentBlock::Text { text: text.into() }],
        origin: UserOrigin::Meta {
            source: "hook_additional_context".into(),
        },
    })
}

fn texts(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user) => match user.content.first() {
                Some(ContentBlock::Text { text }) => Some(text.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// 阻塞：不提交输入、不调模型，回合以 hook 的说明失败。
#[tokio::test]
async fn a_blocking_prompt_hook_keeps_the_input_out() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["unused"])]));
    let hooks = ScriptedHooks::with(
        HookEventName::UserPromptSubmit,
        HookOutcome {
            blocking_error: Some("no".into()),
            notices: vec!["UserPromptSubmit operation blocked by hook:\nno".into()],
            ..HookOutcome::default()
        },
    );
    let mut setup = Setup::new(model.clone());
    setup.hooks = Arc::new(hooks);
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "secret")).await;
    let seen = until(&mut events, finished).await;
    assert!(matches!(
        seen.last(),
        Some(KernelEvent::TurnFinished { outcome: TurnOutcome::Failed { message }, .. })
            if message.starts_with("UserPromptSubmit operation blocked by hook:")
    ));
    assert!(built.session.view().await.unwrap().messages.is_empty());
    assert!(model.requests().is_empty());
}

/// prevent：提交输入与 hook 消息，不调模型；正常：hook 消息跟在输入后面一起发给模型。
#[tokio::test]
async fn prompt_hook_messages_follow_the_input() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["ok"])]));
    let hooks = Arc::new(ScriptedHooks::with(
        HookEventName::UserPromptSubmit,
        HookOutcome {
            prevent_continuation: true,
            messages: vec![meta_message("msg_h1", "Operation stopped by hook")],
            ..HookOutcome::default()
        },
    ));
    let mut setup = Setup::new(model.clone());
    setup.hooks = hooks.clone();
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "first")).await;
    let seen = until(&mut events, finished).await;
    assert!(matches!(
        seen.last(),
        Some(KernelEvent::TurnFinished {
            outcome: TurnOutcome::Success,
            ..
        })
    ));
    assert!(model.requests().is_empty());
    hooks.set(
        HookEventName::UserPromptSubmit,
        HookOutcome {
            messages: vec![meta_message("msg_h2", "extra context")],
            session_title: Some("Named".into()),
            ..HookOutcome::default()
        },
    );
    built.session.send(submit("c2", "second")).await;
    let seen = until(&mut events, finished).await;
    assert!(
        seen.iter()
            .any(|e| matches!(e, KernelEvent::TitleChanged { title } if title == "Named"))
    );
    let view = built.session.view().await.unwrap();
    assert_eq!(
        texts(&view.messages),
        [
            "first",
            "Operation stopped by hook",
            "second",
            "extra context"
        ]
    );
    let request = &model.requests()[0];
    assert_eq!(request.messages.len(), 1, "user turn merged for the API");
    let payload = &hooks.seen()[1].payload;
    assert_eq!(payload["prompt"], "second");
    assert_eq!(hooks.seen()[1].context.mode, Some(view.mode));
}

/// hook 跑很久时 actor 照常响应：view 可用；中断后输入照常落库，接着是中断标记，不调模型（修复线 F37：以前输入
/// 不落库，用户刚发的消息没了；处理输入之后不看中止，early_interrupt_tests.rs）。
#[tokio::test]
async fn an_interrupt_during_the_prompt_hook_keeps_the_input() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["unused"])]));
    let gate = Arc::new(Notify::new());
    let hooks = ScriptedHooks {
        gate: Some(gate),
        ..ScriptedHooks::default()
    };
    hooks.set(HookEventName::UserPromptSubmit, HookOutcome::default());
    let mut setup = Setup::new(model.clone());
    setup.hooks = Arc::new(hooks);
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "slow")).await;
    until(&mut events, |e| {
        matches!(e, KernelEvent::TurnStarted { .. })
    })
    .await;
    let view = built.session.view().await.unwrap();
    assert!(view.running && view.messages.is_empty());
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
        ["slow", "[Request interrupted by user]"]
    );
    assert!(model.requests().is_empty());
}

/// SessionStart：hook 消息排在第一条输入之前，`initial_user_message` 作为第一条输入；启动期间的
/// 输入排在它后面。
#[tokio::test]
async fn session_start_messages_come_before_the_first_input() {
    let model = Arc::new(ScriptedModel::new(vec![
        text_response(&["one"]),
        text_response(&["two"]),
    ]));
    let hooks = Arc::new(ScriptedHooks::with(
        HookEventName::SessionStart,
        HookOutcome {
            messages: vec![meta_message("msg_s1", "start context")],
            initial_user_message: Some("from hook".into()),
            ..HookOutcome::default()
        },
    ));
    let mut setup = Setup::new(model.clone());
    setup.hooks = hooks.clone();
    setup.start = StartSource::Clear;
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "typed")).await;
    until(&mut events, finished).await;
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    assert_eq!(
        texts(&view.messages),
        ["start context", "from hook", "typed"]
    );
    let start = &hooks.seen()[0];
    assert_eq!(start.name, HookEventName::SessionStart);
    assert_eq!(start.payload["source"], "clear");
    assert_eq!(start.payload["model"], "scripted");
    assert_eq!(model.requests().len(), 2);
}

/// SessionEnd 在调用方的任务里跑，返回失败的 hook 的提示。
#[tokio::test]
async fn closing_runs_session_end_with_the_reason() {
    let model = Arc::new(ScriptedModel::new(Vec::new()));
    let hooks = Arc::new(ScriptedHooks::with(
        HookEventName::SessionEnd,
        HookOutcome {
            notices: vec!["SessionEnd hook [x] failed: boom".into()],
            ..HookOutcome::default()
        },
    ));
    let mut setup = Setup::new(model);
    setup.hooks = hooks.clone();
    let built = setup.build().await;
    let notices = built.session.close(EndReason::PromptInputExit).await;
    assert_eq!(notices, ["SessionEnd hook [x] failed: boom"]);
    assert_eq!(hooks.seen()[0].payload["reason"], "prompt_input_exit");
}

/// 输入在回合任务里提交（UserPromptSubmit 之后）：提交失败以 `TurnFinished(Failed)` 报告，
/// 输入不进历史。
#[tokio::test]
async fn a_failed_commit_fails_the_turn_without_the_input() {
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(Vec::new())));
    setup.stored = false;
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    assert!(matches!(
        built.session.send(submit("c1", "hello")).await,
        rust_cli_kernel_api::session_api::Ack::Accepted { .. }
    ));
    let seen = until(&mut events, finished).await;
    assert!(matches!(
        seen.last(),
        Some(KernelEvent::TurnFinished {
            outcome: TurnOutcome::Failed { .. },
            ..
        })
    ));
    assert!(built.session.view().await.unwrap().messages.is_empty());
}

/// 当步的 attachment 是 `turn`，每一步的上下文宣布是 `date`。
struct Phased;

#[async_trait::async_trait]
impl rust_cli_kernel_api::orchestration::ContextBuilder for Phased {
    async fn build(
        &self,
        _request: rust_cli_kernel_api::orchestration::ContextRequest,
        _history: &[Message],
    ) -> Result<
        rust_cli_kernel_api::orchestration::BuiltContext,
        rust_cli_kernel_api::orchestration::OrchestrationError,
    > {
        Ok(rust_cli_kernel_api::orchestration::BuiltContext {
            system: Vec::new(),
            user_context: Vec::new(),
            attachments: vec![attachment("msg_date", "date")],
        })
    }

    async fn turn_attachments(
        &self,
        _request: rust_cli_kernel_api::orchestration::ContextRequest,
        _history: &[Message],
    ) -> Result<
        Vec<rust_cli_kernel_api::message::AttachmentMessage>,
        rust_cli_kernel_api::orchestration::OrchestrationError,
    > {
        Ok(vec![attachment("msg_turn", "total_tokens_reminder")])
    }
}

fn attachment(id: &str, source: &str) -> rust_cli_kernel_api::message::AttachmentMessage {
    rust_cli_kernel_api::message::AttachmentMessage {
        id: rust_cli_kernel_api::ids::MessageId::new(id),
        source: source.into(),
        text: format!("<system-reminder>\n{id}\n</system-reminder>"),
        data: serde_json::Value::Null,
    }
}

/// F10（hooks.md §7）：UserPromptSubmit 的附加上下文交给主循环，排在输入那一步的当步 attachment
/// 之后、上下文宣布之前（先产生输入的 attachment、再跑 UPS）。旧代码在提交输入时就提交 hook
/// 消息，排在当步 attachment 之前。
#[tokio::test]
async fn prompt_hook_context_follows_the_turn_attachments_of_the_input() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["ok"])]));
    let hooks = Arc::new(ScriptedHooks::with(
        HookEventName::UserPromptSubmit,
        HookOutcome {
            messages: vec![Message::Attachment(attachment(
                "msg_ups",
                "hook_additional_context",
            ))],
            ..HookOutcome::default()
        },
    ));
    let mut setup = Setup::new(model.clone());
    setup.hooks = hooks;
    setup.context = Some(Arc::new(Phased));
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "hello")).await;
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    let ids: Vec<&str> = view
        .messages
        .iter()
        .map(|message| message.id().as_str())
        .collect();
    assert_eq!(ids[1..4], ["msg_turn", "msg_ups", "msg_date"], "{ids:?}");
}
