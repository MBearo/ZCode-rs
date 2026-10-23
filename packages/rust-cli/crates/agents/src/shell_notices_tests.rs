//! 子代理里后台命令的通知（docs/specs/rust-cli/agents.md §5.2，L18）：子代理在跑时进它的邮箱、在它的工具
//! 边界并入；它已结束时送顶层会话；交出之后才 ack 执行环境的通知；`-p` 的后台工作算上这些命令。

use std::sync::Arc;

use futures_util::StreamExt;
use rust_cli_kernel_api::agents::AgentSignal;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::testing::{NoticeEnvironment, shell_notice};
use serde_json::json;

use crate::test_support::{Gates, held_with_env, text};

const ROOT: &str = "sess_parent";

#[tokio::test]
async fn a_subagent_command_notice_goes_to_the_subagent_then_to_the_top_level() {
    let gates = Gates::default();
    let env = Arc::new(NoticeEnvironment::default());
    let harness = held_with_env(&gates, false, env.clone());
    let signals = harness.agents.signals();
    let mut root = signals.signals(&SessionId::new(ROOT));
    let launched = harness
        .call(
            ROOT,
            "Agent",
            json!({"description": "Shell owner", "prompt": "Go."}),
        )
        .await;
    let output = text(&launched);
    let id = output
        .split("agentId: ")
        .nth(1)
        .and_then(|rest| rest.split(' ').next())
        .unwrap()
        .to_owned();
    let session = format!("sess_subagent_{id}");
    harness.started(1).await;
    // 子代理在跑：命令通知进它的邮箱，交出之后 ack；它在工具边界取走。
    env.finish(shell_notice(1, "b1"));
    for _ in 0..10_000 {
        if !env.acked().is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(env.acked(), [1]);
    gates.release(&session);
    let Some(AgentSignal::Notice(own)) = root.next().await else {
        panic!("expected the agent's own notice");
    };
    assert_eq!(own.agent.as_str(), id);
    let absorbed = harness.absorbed.lock().unwrap().clone();
    assert_eq!(absorbed.len(), 1, "{absorbed:?}");
    assert_eq!(absorbed[0].0.as_str(), session);
    assert!(
        absorbed[0]
            .1
            .starts_with("<system-reminder>\n[SYSTEM NOTIFICATION")
    );
    assert!(absorbed[0].1.contains("<task-id>b1</task-id>"));
    // 子代理已结束：之后的命令通知送顶层会话。
    env.finish(shell_notice(2, "b2"));
    let Some(AgentSignal::Notice(late)) = root.next().await else {
        panic!("expected the command notice at the top level");
    };
    assert_eq!(late.agent.as_str(), "b2");
    assert!(late.text.contains("<task-id>b2</task-id>"));
    for _ in 0..10_000 {
        if env.acked().len() == 2 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(env.acked(), [1, 2]);
}
