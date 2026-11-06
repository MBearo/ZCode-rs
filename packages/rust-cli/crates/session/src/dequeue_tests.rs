//! 取回排队的输入（`Command::Dequeue`，docs/specs/rust-cli/query.md §4.4）：还在队列里的出队、不再发出；
//! 已经开了回合或并入了回合的拒绝；工具边界领取之后、结局交回之前取回的插话不并入。

use rust_cli_kernel_api::hooks::{HookEventName, HookOutcome};

use super::*;
use crate::lane::ALREADY_SENT;
use crate::test_support::ScriptedHooks;

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

fn dequeue(id: &str, command: &str) -> CommandEnvelope {
    CommandEnvelope {
        id: CommandId::new(id),
        command: Command::Dequeue {
            command: CommandId::new(command),
        },
    }
}

/// 这段文字出现在第几个请求里（没有就是空）。
fn requests_with(model: &ScriptedModel, text: &str) -> Vec<usize> {
    model
        .requests()
        .iter()
        .enumerate()
        .filter(|(_, request)| format!("{:?}", request.messages).contains(text))
        .map(|(at, _)| at)
        .collect()
}

#[tokio::test]
async fn a_queued_input_taken_back_never_goes_out() {
    let scope = TaskScope::root("test", |_| {});
    let (_store, model, session) = asking_session(&scope).await;
    let mut events = session.subscribe(Some(0));
    session.send(submit("c1", "ask me")).await;
    let interaction = asked(&mut events).await;
    session.send(submit("c2", "after this turn")).await;
    assert_eq!(session.view().await.unwrap().queue.len(), 1);
    assert_eq!(
        session.send(dequeue("c3", "c2")).await,
        Ack::Accepted {
            result: rust_cli_kernel_api::session_api::CommandResult::Done
        }
    );
    let view = session.view().await.unwrap();
    assert!(view.queue.is_empty());
    session.send(answer("c4", interaction)).await;
    until_finished(&mut events, 1).await;
    // 回合结束后没有它的回合：只有本回合的两次请求，对话里也没有它。
    assert_eq!(model.requests().len(), 2);
    assert!(requests_with(&model, "after this turn").is_empty());
    let messages = session.view().await.unwrap().messages;
    assert!(!format!("{messages:?}").contains("after this turn"));
}

#[tokio::test]
async fn a_steer_taken_back_before_the_tool_boundary_is_not_absorbed() {
    let scope = TaskScope::root("test", |_| {});
    let (_store, model, session) = asking_session(&scope).await;
    let mut events = session.subscribe(Some(0));
    session.send(submit("c1", "ask me")).await;
    let interaction = asked(&mut events).await;
    session.send(steer("c2", "also b")).await;
    assert!(matches!(
        session.send(dequeue("c3", "c2")).await,
        Ack::Accepted { .. }
    ));
    session.send(answer("c4", interaction)).await;
    until_finished(&mut events, 1).await;
    assert!(requests_with(&model, "also b").is_empty());
    assert!(session.view().await.unwrap().queue.is_empty());
}

#[tokio::test]
async fn an_input_that_already_went_out_cannot_be_taken_back() {
    let scope = TaskScope::root("test", |_| {});
    let (_store, _model, session) = asking_session(&scope).await;
    let mut events = session.subscribe(Some(0));
    // 空闲时提交的输入马上开回合，不进队列。
    session.send(submit("c1", "ask me")).await;
    asked(&mut events).await;
    assert_eq!(
        session.send(dequeue("c2", "c1")).await,
        Ack::Rejected {
            reason: ALREADY_SENT.to_owned()
        }
    );
    // 没有这个命令 ID 的输入：同样拒绝。
    assert_eq!(
        session.send(dequeue("c3", "nope")).await,
        Ack::Rejected {
            reason: ALREADY_SENT.to_owned()
        }
    );
}

#[tokio::test]
async fn an_absorbed_steer_cannot_be_taken_back() {
    let scope = TaskScope::root("test", |_| {});
    let (_store, model, session) = asking_session(&scope).await;
    let mut events = session.subscribe(Some(0));
    session.send(submit("c1", "ask me")).await;
    let interaction = asked(&mut events).await;
    session.send(steer("c2", "also b")).await;
    session.send(answer("c3", interaction)).await;
    until_finished(&mut events, 1).await;
    assert_eq!(requests_with(&model, "also b"), [1]);
    assert!(matches!(
        session.send(dequeue("c4", "c2")).await,
        Ack::Rejected { .. }
    ));
}

/// 领取之后、结局交回之前取回（UPS hook 还在跑）：插话回到了用户手里，本回合不能再并入它（以前并入了，
/// 同一条消息既回到输入框又发了出去）。
#[tokio::test]
async fn a_steer_taken_back_while_its_hook_runs_is_not_absorbed() {
    let scope = TaskScope::root("test", |_| {});
    let gate = Arc::new(Notify::new());
    let entered = Arc::new(Notify::new());
    let hooks = Arc::new(ScriptedHooks {
        gate: Some(gate.clone()),
        entered: Some(entered.clone()),
        ..ScriptedHooks::default()
    });
    hooks.set(HookEventName::UserPromptSubmit, HookOutcome::default());
    let (_store, model, session) = asking_session_with(&scope, hooks.clone()).await;
    let mut events = session.subscribe(Some(0));
    session.send(submit("c1", "ask me")).await;
    // 第一条输入自己的 UPS：放行。
    entered.notified().await;
    gate.notify_one();
    let interaction = asked(&mut events).await;
    session.send(steer("c2", "also b")).await;
    session.send(answer("c3", interaction)).await;
    // 工具边界领取了插话，它的 UPS 停在闸门上。
    entered.notified().await;
    assert!(matches!(
        session.send(dequeue("c4", "c2")).await,
        Ack::Accepted { .. }
    ));
    gate.notify_one();
    until_finished(&mut events, 1).await;
    assert_eq!(model.requests().len(), 2);
    assert!(requests_with(&model, "also b").is_empty());
    let view = session.view().await.unwrap();
    assert!(view.queue.is_empty());
    assert!(!view.messages.iter().any(|message| matches!(
        message,
        Message::Attachment(attachment) if attachment.source == "queued_command"
    )));
}
