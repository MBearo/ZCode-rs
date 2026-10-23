//! 插话（`Delivery::Steer`）在工具边界并入当前回合（docs/specs/rust-cli/query.md §4）。

use super::*;

fn steer(id: &str, text: &str) -> CommandEnvelope {
    CommandEnvelope {
        id: CommandId::new(id),
        command: Command::Submit {
            text: text.to_owned(),
            attachments: Vec::new(),
            delivery: Delivery::Steer,
        },
    }
}

#[tokio::test]
async fn a_steering_input_joins_the_running_turn_and_a_queued_one_waits() {
    let scope = TaskScope::root("test", |_| {});
    let (_store, model, session) = asking_session(&scope).await;
    let mut events = session.subscribe(Some(0));
    session.send(submit("c1", "ask me")).await;
    let interaction = asked(&mut events).await;
    // 工具还在等回答：一条插话、一条排队。
    session.send(steer("c2", "also b")).await;
    session.send(submit("c3", "next turn")).await;
    assert_eq!(session.view().await.unwrap().queue.len(), 2);
    session.send(answer("c4", interaction)).await;
    until_finished(&mut events, 1).await;
    // 插话进了本回合的第二次请求；排队的输入留到回合结束后。
    let requests = model.requests();
    assert!(format!("{:?}", requests[1].messages).contains("also b"));
    assert!(!format!("{:?}", requests[1].messages).contains("next turn"));
    let messages = session.view().await.unwrap().messages;
    assert!(messages.iter().any(|message| matches!(
        message,
        Message::Attachment(attachment) if attachment.source == "queued_command"
    )));
}

/// 斜杠命令不在工具边界并入（query.md §4）：后面的插话照常并入，
/// 斜杠命令留到回合结束后作为独立回合。
#[tokio::test]
async fn a_slash_command_is_not_absorbed_mid_turn() {
    let scope = TaskScope::root("test", |_| {});
    let (_store, model, session) = asking_session(&scope).await;
    let mut events = session.subscribe(Some(0));
    session.send(submit("c1", "ask me")).await;
    let interaction = asked(&mut events).await;
    session.send(steer("c2", "  /greet Ada")).await;
    session.send(steer("c3", "also b")).await;
    session.send(answer("c4", interaction)).await;
    until_finished(&mut events, 2).await;
    let requests = model.requests();
    let second = format!("{:?}", requests[1].messages);
    assert!(second.contains("also b"), "{second}");
    assert!(!second.contains("/greet Ada"), "{second}");
    // 回合结束后它自己开一个回合（输入原样提交）。
    let messages = session.view().await.unwrap().messages;
    assert!(messages.iter().any(|message| matches!(
        message,
        Message::User(user) if user.origin == rust_cli_kernel_api::message::UserOrigin::Human
            && format!("{:?}", user.content).contains("/greet Ada")
    )));
}

mod user_prompt_submit {
    //! 插话在并入点跑 UserPromptSubmit（query.md §4.1）：并入时 hook 消息跟在 `queued_command` 之后；
    //! 阻塞的丢弃；`prevent_continuation` 的留到回合结束后，作为独立回合且不再跑 hook。

    use rust_cli_kernel_api::hooks::{HookEventName, HookOutcome};
    use rust_cli_kernel_api::ids::MessageId;
    use rust_cli_kernel_api::message::AttachmentMessage;

    use super::*;
    use crate::test_support::ScriptedHooks;

    fn context(text: &str) -> Message {
        Message::Attachment(AttachmentMessage {
            id: MessageId::new(format!("hook_{text}")),
            source: "hook_additional_context".to_owned(),
            text: text.to_owned(),
            data: serde_json::Value::Null,
        })
    }

    /// 回合在等工具的回答时：设好插话的 UPS 结果，提交一条插话，再回答。
    async fn steer_with(
        outcome: HookOutcome,
    ) -> (Arc<ScriptedHooks>, Arc<ScriptedModel>, Session, TaskScope) {
        let scope = TaskScope::root("test", |_| {});
        let hooks = Arc::new(ScriptedHooks::default());
        let (_store, model, session) = asking_session_with(&scope, hooks.clone()).await;
        let mut events = session.subscribe(Some(0));
        session.send(submit("c1", "ask me")).await;
        let interaction = asked(&mut events).await;
        hooks.set(HookEventName::UserPromptSubmit, outcome);
        session.send(steer("c2", "also b")).await;
        session.send(answer("c3", interaction)).await;
        until_finished(&mut events, 1).await;
        (hooks, model, session, scope)
    }

    fn prompts_seen(hooks: &ScriptedHooks, prompt: &str) -> usize {
        hooks
            .seen()
            .iter()
            .filter(|event| {
                event.name == HookEventName::UserPromptSubmit && event.payload["prompt"] == prompt
            })
            .count()
    }

    #[tokio::test]
    async fn an_absorbed_steer_is_followed_by_its_hook_messages() {
        let (hooks, model, session, _scope) = steer_with(HookOutcome {
            messages: vec![context("steer ctx")],
            ..HookOutcome::default()
        })
        .await;
        assert_eq!(prompts_seen(&hooks, "also b"), 1);
        let messages = session.view().await.unwrap().messages;
        let at = messages
            .iter()
            .position(
                |message| matches!(message, Message::Attachment(a) if a.source == "queued_command"),
            )
            .unwrap();
        assert_eq!(messages[at + 1].id().as_str(), "hook_steer ctx");
        assert!(format!("{:?}", model.requests()[1].messages).contains("steer ctx"));
        assert!(session.view().await.unwrap().queue.is_empty());
    }

    /// 丢弃时给界面一条提示（工具边界先给出 notices，query.md §4.2，L18）：warning 级别，
    /// 文本是 hooks crate 渲染的 `notices[0]`，排在这次工具边界之后的请求之前。
    #[tokio::test]
    async fn a_blocked_steer_shows_a_warning_at_the_tool_boundary() {
        let scope = TaskScope::root("test", |_| {});
        let hooks = Arc::new(ScriptedHooks::default());
        let (_store, _model, session) = asking_session_with(&scope, hooks.clone()).await;
        let mut events = session.subscribe(Some(0));
        session.send(submit("c1", "ask me")).await;
        let interaction = asked(&mut events).await;
        let warning = "UserPromptSubmit operation blocked by hook:\nno\n\nOriginal prompt: also b";
        hooks.set(
            HookEventName::UserPromptSubmit,
            HookOutcome {
                blocking_error: Some("no".to_owned()),
                notices: vec![warning.to_owned()],
                ..HookOutcome::default()
            },
        );
        session.send(steer("c2", "also b")).await;
        session.send(answer("c3", interaction)).await;
        let seen = until_finished(&mut events, 1).await;
        let notice = seen
            .iter()
            .position(|event| {
                matches!(event, KernelEvent::SystemNotice { level, text }
                    if *level == rust_cli_kernel_api::session_api::NoticeLevel::Warning && text == warning)
            })
            .expect("a warning for the blocked steer");
        let reply = seen
            .iter()
            .position(|event| {
                matches!(event, KernelEvent::MessageDone { message: Message::Assistant(a) }
                    if format!("{:?}", a.content).contains("thanks"))
            })
            .unwrap();
        assert!(notice < reply, "{seen:?}");
    }

    #[tokio::test]
    async fn a_blocked_steer_is_dropped() {
        let (_hooks, model, session, _scope) = steer_with(HookOutcome {
            blocking_error: Some("no".to_owned()),
            ..HookOutcome::default()
        })
        .await;
        assert!(!format!("{:?}", model.requests()[1].messages).contains("also b"));
        let view = session.view().await.unwrap();
        assert!(view.queue.is_empty());
        assert!(!view.messages.iter().any(|message| {
            matches!(message, Message::Attachment(a) if a.source == "queued_command")
        }));
    }

    #[tokio::test]
    async fn a_stopped_steer_waits_for_its_own_turn_without_rerunning_the_hook() {
        let scope = TaskScope::root("test", |_| {});
        let hooks = Arc::new(ScriptedHooks::default());
        let (_store, model, session) = asking_session_with(&scope, hooks.clone()).await;
        let mut events = session.subscribe(Some(0));
        session.send(submit("c1", "ask me")).await;
        let interaction = asked(&mut events).await;
        hooks.set(
            HookEventName::UserPromptSubmit,
            HookOutcome {
                prevent_continuation: true,
                messages: vec![context("stopped")],
                ..HookOutcome::default()
            },
        );
        session.send(steer("c2", "also b")).await;
        session.send(answer("c3", interaction)).await;
        let seen = until_finished(&mut events, 2).await;
        // 本回合没有并入它；之后它自己一个回合：输入与 hook 消息落库，不调模型、不再跑 hook。
        assert!(!format!("{:?}", model.requests()[1].messages).contains("also b"));
        assert_eq!(model.requests().len(), 2);
        assert_eq!(prompts_seen(&hooks, "also b"), 1);
        let outcomes: Vec<&TurnOutcome> = seen
            .iter()
            .filter_map(|event| match event {
                KernelEvent::TurnFinished { outcome, .. } => Some(outcome),
                _ => None,
            })
            .collect();
        assert_eq!(outcomes, [&TurnOutcome::Success, &TurnOutcome::Success]);
        let messages = session.view().await.unwrap().messages;
        let texts = format!("{messages:?}");
        assert!(
            texts.contains("also b") && texts.contains("hook_stopped"),
            "{texts}"
        );
    }
}
