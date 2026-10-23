//! 会话 actor 提交的计量（docs/specs/rust-cli/usage.md §2.5）：计量与它的消息相邻（同一批），回合结束写
//! `TurnEnded`。
use std::sync::Arc;

use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::message::{Message, UserOrigin};
use rust_cli_kernel_api::metering::CallStatus;
use rust_cli_kernel_api::session_api::SessionHandle;
use rust_cli_kernel_api::store::Fact;
use rust_cli_kernel_api::testing::{FakeTool, ScriptedModel, text_response, tool_use_response};
use serde_json::json;

use crate::test_support::{Setup, finished, submit, until};

/// 事实的种类（`MessageAppended` 按消息种类细分）。
fn kinds(facts: &[Fact]) -> Vec<String> {
    facts
        .iter()
        .filter_map(|fact| {
            Some(match fact {
                Fact::MessageAppended { message } => match message {
                    Message::Assistant(_) => "assistant".to_owned(),
                    Message::User(user) => match user.origin {
                        UserOrigin::ToolResult { .. } => "tool_result".to_owned(),
                        UserOrigin::Human => "human".to_owned(),
                        _ => return None,
                    },
                    _ => return None,
                },
                Fact::ModelCalled { .. } => "model_called".to_owned(),
                Fact::ToolCalled { .. } => "tool_called".to_owned(),
                Fact::TurnEnded { status } => format!("turn_ended:{status:?}"),
                _ => return None,
            })
        })
        .collect()
}

#[tokio::test]
async fn calls_follow_their_messages_and_the_turn_end_comes_last() {
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Echo", &json!({})),
        text_response(&["done"]),
    ]));
    let mut setup = Setup::new(model);
    setup.tools = vec![Arc::new(FakeTool::new("Echo", true, "ok"))];
    let built = setup.build().await;
    let mut events = built.session.subscribe(None);
    built.session.send(submit("c1", "hi")).await;
    until(&mut events, finished).await;
    let facts = built.store.facts(&SessionId::new("sess_1"));
    assert_eq!(
        kinds(&facts),
        [
            "human",
            "assistant",
            "model_called",
            "tool_result",
            "tool_called",
            "assistant",
            "model_called",
            "turn_ended:Completed"
        ]
    );
    // assistant 消息与它的计量是同一次请求。
    let pairs: Vec<_> = facts
        .windows(2)
        .filter_map(|pair| match pair {
            [
                Fact::MessageAppended {
                    message: Message::Assistant(assistant),
                },
                Fact::ModelCalled { call },
            ] => Some((assistant.id.clone(), call.message.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(pairs.len(), 2);
    assert!(pairs.iter().all(|(message, call)| message == call));
}

#[tokio::test]
async fn a_text_turn_ends_with_one_turn_fact() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["partial"])]));
    let built = Setup::new(model).build().await;
    let mut events = built.session.subscribe(None);
    built.session.send(submit("c1", "hi")).await;
    until(&mut events, finished).await;
    let facts = built.store.facts(&SessionId::new("sess_1"));
    assert!(matches!(
        facts.last(),
        Some(Fact::TurnEnded {
            status: CallStatus::Completed
        })
    ));
}
