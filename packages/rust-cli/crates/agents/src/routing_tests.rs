//! 嵌套后台子代理的通知（docs/specs/rust-cli/agents.md §5.1）：在跑的 owner 在工具边界收到；owner
//! 跑完但还有后台子代理时停放、收到通知后续跑；`-p` 不停放，owner 已结束时送顶层会话；前台子代理
//! 转成后台；停放的子代理可以被 TaskStop 停止。

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rust_cli_kernel_api::agents::{AgentNotice, AgentSignal};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::message::{ContentBlock, Message, UserOrigin};
use rust_cli_kernel_api::store::SessionStore;
use serde_json::json;

use crate::test_support::{Gates, Harness, held, text};

const ROOT: &str = "sess_parent";

/// 一个子代理：ID 与子会话。
struct Spawned {
    id: String,
    session: String,
}

/// “已启动”结果里的 agentId（测试的 ID 生成器与消息共用一个计数器，不能猜）。
fn spawned(output: &str) -> Spawned {
    let id = output
        .split("agentId: ")
        .nth(1)
        .and_then(|rest| rest.split(' ').next())
        .unwrap()
        .to_owned();
    Spawned {
        session: format!("sess_subagent_{id}"),
        id,
    }
}

/// 主线程派生后台 P，P 里再派生后台 C；两者都在等放行。
async fn nested(harness: &Harness) -> (Spawned, Spawned) {
    let launched = harness
        .call(
            ROOT,
            "Agent",
            json!({"description": "Outer", "prompt": "Go."}),
        )
        .await;
    let outer = spawned(&text(&launched));
    harness.started(1).await;
    let launched = harness
        .call(
            &outer.session,
            "Agent",
            json!({"description": "Inner", "prompt": "Dig."}),
        )
        .await;
    let inner = spawned(&text(&launched));
    harness.started(2).await;
    (outer, inner)
}

async fn next_notice(stream: &mut BoxStream<'static, AgentSignal>) -> AgentNotice {
    loop {
        if let Some(AgentSignal::Notice(notice)) = stream.next().await {
            return notice;
        }
    }
}

fn absorbed(harness: &Harness) -> Vec<(SessionId, String)> {
    harness.absorbed.lock().unwrap().clone()
}

async fn background(harness: &Harness) -> Vec<String> {
    let signals = harness.agents.signals();
    let mut agents: Vec<String> = signals
        .background(&SessionId::new(ROOT))
        .await
        .into_iter()
        .map(|agent| agent.to_string())
        .collect();
    agents.sort();
    agents
}

fn sorted(ids: &[&String]) -> Vec<String> {
    let mut ids: Vec<String> = ids.iter().map(|id| (*id).clone()).collect();
    ids.sort();
    ids
}

async fn listed(harness: &Harness) -> String {
    text(&harness.call(ROOT, "ListAgents", json!({})).await)
}

/// 等到 P 停放（后台运行在自己的任务里推进）。
async fn parked(harness: &Harness, outer: &Spawned) {
    let row = format!("  {}  ·  general-purpose  ·  completed", outer.id);
    for _ in 0..10_000 {
        if listed(harness).await.contains(&row) {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("the outer agent did not park");
}

#[tokio::test]
async fn a_running_owner_takes_the_notice_at_its_tool_boundary() {
    let gates = Gates::default();
    let harness = held(&gates, true);
    let mut root = harness.agents.signals().signals(&SessionId::new(ROOT));
    let (p, c) = nested(&harness).await;
    // 子代理里派生时按参数在后台跑；深度 2 的后台子代理仍有 Agent。
    assert!(harness.seen()[1].tools.contains(&"Agent".to_owned()));
    assert_eq!(background(&harness).await, sorted(&[&p.id, &c.id]));
    gates.release(&c.session);
    // C 的通知进了 P 的邮箱，不送顶层会话；P 放行后在工具边界取走。
    gates.release(&p.session);
    let notice = next_notice(&mut root).await;
    assert_eq!(notice.agent.as_str(), p.id);
    let taken = absorbed(&harness);
    assert_eq!(taken.len(), 1, "{taken:?}");
    assert_eq!(taken[0].0.as_str(), p.session);
    assert!(
        taken[0]
            .1
            .starts_with("<system-reminder>\n[SYSTEM NOTIFICATION")
    );
    assert!(taken[0].1.contains(&format!("<task-id>{}</task-id>", c.id)));
    assert!(
        taken[0]
            .1
            .contains("<summary>Agent \"Inner\" finished</summary>")
    );
}

#[tokio::test]
async fn an_owner_with_live_children_parks_and_resumes_on_their_notice() {
    let gates = Gates::default();
    let harness = held(&gates, true);
    let signals = harness.agents.signals();
    let mut root = signals.signals(&SessionId::new(ROOT));
    let (p, c) = nested(&harness).await;
    // P 先跑完：C 还在跑，P 停放，自己的通知推迟。
    gates.release(&p.session);
    parked(&harness, &p).await;
    assert_eq!(background(&harness).await, sorted(&[&p.id, &c.id]));
    // C 跑完：P 续跑，第一条消息是 meta 通知。
    gates.release(&c.session);
    harness.started(3).await;
    let resumed = harness.seen()[2].request.clone();
    assert_eq!(resumed.session.as_str(), p.session);
    let Some(Message::User(first)) = resumed.messages.last() else {
        panic!("expected the notice message");
    };
    assert_eq!(
        first.origin,
        UserOrigin::Meta {
            source: "task-notification".to_owned()
        }
    );
    let [ContentBlock::Text { text: prompt }] = first.content.as_slice() else {
        panic!("expected one text block");
    };
    assert!(prompt.starts_with("<system-reminder>\n[SYSTEM NOTIFICATION"));
    assert!(prompt.contains(&format!("<task-id>{}</task-id>", c.id)));
    // 续跑的消息落在 P 的子会话里。
    let stored = harness
        .store
        .load(&SessionId::new(&p.session))
        .await
        .unwrap();
    assert!(
        stored
            .messages
            .iter()
            .any(|message| *message.id() == first.id)
    );
    // 续跑结束、没有在跑的后台子代理：P 的通知送顶层会话；C 的通知只给了 P。
    gates.release(&p.session);
    let notice = next_notice(&mut root).await;
    assert_eq!(notice.agent.as_str(), p.id);
    assert!(
        notice
            .text
            .contains("<summary>Agent \"Outer\" finished</summary>")
    );
    signals.ack(&SessionId::new(ROOT), notice.seq).await;
    assert!(background(&harness).await.is_empty());
}

#[tokio::test]
async fn print_mode_does_not_park_and_late_notices_go_to_the_top_level() {
    let gates = Gates::default();
    let harness = held(&gates, false);
    let signals = harness.agents.signals();
    let mut root = signals.signals(&SessionId::new(ROOT));
    let (p, c) = nested(&harness).await;
    gates.release(&p.session);
    let outer = next_notice(&mut root).await;
    assert_eq!(outer.agent.as_str(), p.id);
    signals.ack(&SessionId::new(ROOT), outer.seq).await;
    // C 还在跑：`-p` 仍算后台工作（它的通知会送到顶层会话）。
    assert_eq!(background(&harness).await, std::slice::from_ref(&c.id));
    gates.release(&c.session);
    let inner = next_notice(&mut root).await;
    assert_eq!(inner.agent.as_str(), c.id);
    assert!(
        inner
            .text
            .contains("<summary>Agent \"Inner\" finished</summary>")
    );
    assert!(absorbed(&harness).is_empty());
}

#[tokio::test]
async fn a_foreground_owner_with_live_children_becomes_a_background_agent() {
    let gates = Gates::default();
    let harness = held(&gates, true);
    let mut root = harness.agents.signals().signals(&SessionId::new(ROOT));
    let foreground = harness.call(
        ROOT,
        "Agent",
        json!({"description": "Outer", "prompt": "Go.", "run_in_background": false}),
    );
    let drive = async {
        let first = harness.started(1).await;
        let outer = first[0].request.session.to_string();
        let launched = harness
            .call(
                &outer,
                "Agent",
                json!({"description": "Inner", "prompt": "Dig."}),
            )
            .await;
        let inner = spawned(&text(&launched));
        harness.started(2).await;
        gates.release(&outer);
        inner
    };
    let (output, c) = tokio::join!(foreground, drive);
    // 前台调用回“已启动”（`async_launched`），P 转成后台并停放。
    assert!(text(&output).starts_with("Async agent launched successfully."));
    let p = spawned(&text(&output));
    assert!(
        listed(&harness)
            .await
            .contains(&format!("  {}  ·  general-purpose  ·  completed", p.id))
    );
    gates.release(&c.session);
    harness.started(3).await;
    gates.release(&p.session);
    let notice = next_notice(&mut root).await;
    assert_eq!(notice.agent.as_str(), p.id);
}

#[tokio::test]
async fn task_stop_ends_a_parked_agent_with_its_last_report() {
    let gates = Gates::default();
    let harness = held(&gates, true);
    let mut root = harness.agents.signals().signals(&SessionId::new(ROOT));
    let (p, c) = nested(&harness).await;
    gates.release(&p.session);
    parked(&harness, &p).await;
    let stopped = harness
        .call(ROOT, "TaskStop", json!({"task_id": p.id}))
        .await;
    assert!(!stopped.is_error);
    assert!(text(&stopped).contains("\"task_type\":\"local_agent\""));
    // 摘要是 `was stopped by user`，带最后的报告与用量。
    let notice = next_notice(&mut root).await;
    assert_eq!(
        (notice.agent.as_str(), notice.status.as_str()),
        (p.id.as_str(), "killed")
    );
    assert!(
        notice
            .text
            .contains("<summary>Agent \"Outer\" was stopped by user</summary>")
    );
    assert!(
        notice
            .text
            .contains("<result>Hello from the subagent.</result>")
    );
    assert!(notice.text.contains("<usage>"));
    // C 结束时 owner 已结束：通知送顶层会话。
    gates.release(&c.session);
    let inner = next_notice(&mut root).await;
    assert_eq!(inner.agent.as_str(), c.id);
}

/// L18（结束时还有排着的消息）：SendMessage 排进来、
/// 运行结束前没取走的消息：照常结束并送出通知，再用第一条消息在后台续跑（人类来源的 user 消息）。
#[tokio::test]
async fn a_message_left_in_the_mailbox_resumes_the_agent_after_it_ends() {
    let gates = Gates::default();
    let harness = held(&gates, false);
    let mut root = harness.agents.signals().signals(&SessionId::new(ROOT));
    let launched = harness
        .call(
            ROOT,
            "Agent",
            json!({"description": "Worker", "prompt": "Go."}),
        )
        .await;
    let worker = spawned(&text(&launched));
    harness.started(1).await;
    let sent = harness
        .call(
            ROOT,
            "SendMessage",
            json!({"to": worker.id, "message": "Also check the logs."}),
        )
        .await;
    assert!(
        text(&sent).starts_with("Message queued for delivery"),
        "{}",
        text(&sent)
    );
    // 这次运行没有取走消息（假的子循环只在工具边界取通知）。
    gates.release(&worker.session);
    let first = next_notice(&mut root).await;
    assert!(
        first
            .text
            .contains("<summary>Agent \"Worker\" finished</summary>")
    );
    harness.started(2).await;
    let resumed = harness.seen()[1].request.clone();
    assert_eq!(resumed.session.as_str(), worker.session);
    let Some(Message::User(message)) = resumed.messages.last() else {
        panic!("expected the stranded message");
    };
    assert_eq!(message.origin, UserOrigin::Human);
    assert!(
        matches!(message.content.as_slice(), [ContentBlock::Text { text }]
        if text == "The coordinator sent a message while you were working:\nAlso check the logs.\n\nAddress this before completing your current task.")
    );
    gates.release(&worker.session);
    let second = next_notice(&mut root).await;
    assert_eq!(second.agent.as_str(), worker.id);
    assert_ne!(second.seq, first.seq);
    // 续跑不属于哪次工具调用（A8）：第二次通知没有 `<tool-use-id>`。
    assert!(first.text.contains("<tool-use-id>"));
    assert!(!second.text.contains("<tool-use-id>"));
}
