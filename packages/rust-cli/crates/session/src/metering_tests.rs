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

/// 把响应的 `MessageDelta` 换成带输出 token 的用量。
fn with_output(
    mut events: Vec<
        Result<rust_cli_kernel_api::model::ModelEvent, rust_cli_kernel_api::model::ModelError>,
    >,
    output: u64,
) -> Vec<Result<rust_cli_kernel_api::model::ModelEvent, rust_cli_kernel_api::model::ModelError>> {
    use rust_cli_kernel_api::model::ModelEvent;
    for event in &mut events {
        if let Ok(ModelEvent::MessageDelta { usage, .. }) = event {
            usage.output_tokens = output;
        }
    }
    events
}

/// 修复线 F105（tui-display.md §2.6）：每个完成的主回合请求提交之后，actor 发 `OutputSpeed` 并记下最近一个，会话的
/// 快照（`SessionView.output_speed`）带它；只有工具调用的请求也有（首个内容是 tool_use 块开始）。旧代码没有这个事件。
#[tokio::test]
async fn completed_main_requests_report_their_output_speed() {
    use rust_cli_kernel_api::metering::OutputSpeed;
    use rust_cli_kernel_api::session_api::KernelEvent;

    let model = Arc::new(ScriptedModel::new(vec![
        with_output(tool_use_response("toolu_1", "Echo", &json!({})), 7),
        with_output(text_response(&["done"]), 3),
    ]));
    let mut setup = Setup::new(model);
    setup.tools = vec![Arc::new(FakeTool::new("Echo", true, "ok"))];
    let built = setup.build().await;
    assert_eq!(built.session.view().await.unwrap().output_speed, None);
    let mut events = built.session.subscribe(None);
    built.session.send(submit("c1", "hi")).await;
    let seen = until(&mut events, finished).await;
    let speeds: Vec<OutputSpeed> = seen
        .iter()
        .filter_map(|event| match event {
            KernelEvent::OutputSpeed { speed } => Some(*speed),
            _ => None,
        })
        .collect();
    // 测试的时钟固定在 0：时长与首个内容都是 0（速度本身的算法见 kernel-api 的测试）。
    let speed = |output| OutputSpeed {
        output_tokens: Some(output),
        duration_ms: 0,
        first_content_ms: Some(0),
    };
    assert_eq!(speeds, [speed(7), speed(3)]);
    assert_eq!(
        built.session.view().await.unwrap().output_speed,
        Some(speed(3))
    );
    // 用量表的首字照旧：只有工具调用的那次请求没有首字。
    let facts = built.store.facts(&SessionId::new("sess_1"));
    let firsts: Vec<_> = facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::ModelCalled { call } => Some(call.first_token_at_ms),
            _ => None,
        })
        .collect();
    assert_eq!(firsts, [None, Some(0)]);
}

/// 只有完成的主回合请求更新速度：中断、失败、子代理、旁路与压缩请求都不更新。
#[test]
fn only_completed_main_requests_have_an_output_speed() {
    use rust_cli_kernel_api::ids::MessageId;
    use rust_cli_kernel_api::message::Usage;
    use rust_cli_kernel_api::metering::{ModelCall, OutputSpeed};
    use rust_cli_kernel_api::model::QuerySource;
    use rust_cli_kernel_api::testing::selection;

    use crate::turn::output_speed;

    let call = ModelCall {
        message: MessageId::new("msg_1"),
        source: QuerySource::MainThread,
        selection: selection(),
        started_at_ms: 1_000,
        first_token_at_ms: None,
        first_content_at_ms: Some(1_400),
        completed_at_ms: 3_000,
        status: CallStatus::Completed,
        stop_reason: None,
        tool_calls: 1,
        usage: Some(Usage {
            output_tokens: 80,
            ..Usage::default()
        }),
        retries: 0,
        error: None,
    };
    let speed = output_speed(&call).expect("a completed main request");
    assert_eq!(
        speed,
        OutputSpeed {
            output_tokens: Some(80),
            duration_ms: 2_000,
            first_content_ms: Some(400),
        }
    );
    assert_eq!(speed.tokens_per_second(), Some(50.0));
    for status in [CallStatus::Cancelled, CallStatus::Error] {
        assert_eq!(
            output_speed(&ModelCall {
                status,
                ..call.clone()
            }),
            None
        );
    }
    for source in [
        QuerySource::Subagent("general-purpose".into()),
        QuerySource::Compact,
        QuerySource::Side("generate_session_title".into()),
    ] {
        assert_eq!(
            output_speed(&ModelCall {
                source,
                ..call.clone()
            }),
            None
        );
    }
    // 用量未知：有速度的记录，但算不出 TPS。
    let unknown = output_speed(&ModelCall {
        usage: None,
        ..call
    })
    .unwrap();
    assert_eq!(unknown.tokens_per_second(), None);
}
