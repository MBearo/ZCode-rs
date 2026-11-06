//! 子代理里后台命令的通知（docs/specs/rust-cli/agents.md §5.2，L18）：子代理在跑时进它的邮箱、在它的工具
//! 边界并入；它已结束时送顶层会话；交出之后才 ack 执行环境的通知；`-p` 的后台工作算上这些命令。

use std::sync::Arc;

use futures_util::StreamExt;
use rust_cli_kernel_api::agents::AgentSignal;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::testing::{NoticeEnvironment, machine, shell_notice};
use serde_json::json;

use crate::test_support::{Gates, held_with_env, text};

const ROOT: &str = "sess_parent";

#[tokio::test]
async fn a_subagent_command_notice_goes_to_the_subagent_then_to_the_top_level() {
    let gates = Gates::default();
    let env = Arc::new(NoticeEnvironment::default());
    let harness = held_with_env(&gates, false, machine(env.clone()));
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

// 命令保活（agents.md §5.2 第 5 条，用户决定 2026-11-05）：后台子代理跑完时（交互模式与 `-p`），子会话里还有没交给它的后台命令
// 就停放、现在送出自己的通知（keepalive 的 note），命令的通知让它续跑，而不是送顶层会话。修复前它直接结束，命令的
// 通知送了顶层会话，子代理再也不醒（2026-11-04 的长任务里两个子代理就这样停住）。

const KEEPALIVE: &str = "<note>This agent stopped with background work of its own still running.";
const PLAIN: &str = "<note>A task-notification fires each time this agent stops";

/// 后台子代理与它的子会话。
async fn launch(harness: &crate::test_support::Harness) -> (String, String) {
    let launched = harness
        .call(
            ROOT,
            "Agent",
            json!({"description": "Builder", "prompt": "Build.", "run_in_background": true}),
        )
        .await;
    let id = text(&launched)
        .split("agentId: ")
        .nth(1)
        .and_then(|rest| rest.split(' ').next())
        .unwrap()
        .to_owned();
    let session = format!("sess_subagent_{id}");
    harness.started(1).await;
    (id, session)
}

async fn next_notice(
    stream: &mut futures_util::stream::BoxStream<'static, AgentSignal>,
) -> rust_cli_kernel_api::agents::AgentNotice {
    loop {
        if let Some(AgentSignal::Notice(notice)) = stream.next().await {
            return notice;
        }
    }
}

/// 顶层会话里还在跑或停放的后台子代理（停放的也在；不含命令）。
async fn live(harness: &crate::test_support::Harness) -> Vec<String> {
    harness
        .agents
        .core
        .live
        .background_in(&SessionId::new(ROOT))
        .into_iter()
        .map(|agent| agent.agent.to_string())
        .collect()
}

/// 续跑的那次运行的最后一条消息（续跑的第一条消息）。
fn resumed_with(harness: &crate::test_support::Harness, run: usize) -> String {
    let request = harness.seen()[run].request.clone();
    let Some(rust_cli_kernel_api::message::Message::User(first)) = request.messages.last() else {
        panic!("expected the notice message");
    };
    let [rust_cli_kernel_api::message::ContentBlock::Text { text }] = first.content.as_slice()
    else {
        panic!("expected one text block");
    };
    text.clone()
}

#[tokio::test]
async fn a_subagent_with_a_running_command_parks_and_the_command_wakes_it() {
    parks_and_wakes(true).await;
}

// `-p` 同样（用户决定 2026-11-05，agents.md §5.2 第 5 条）：以前 `-p` 里它直接结束、命令的通知送顶层会话。停放的子代理算 `-p` 的后台工作（`background_in`），所以 `-p` 等它续跑完才收尾。
#[tokio::test]
async fn in_print_mode_a_subagent_with_a_running_command_parks_too() {
    parks_and_wakes(false).await;
}

async fn parks_and_wakes(keep_alive: bool) {
    let gates = Gates::default();
    let env = Arc::new(NoticeEnvironment::default());
    env.finish_after_checks(shell_notice(1, "b1"), usize::MAX);
    let harness = held_with_env(&gates, keep_alive, machine(env.clone()));
    let mut root = harness.agents.signals().signals(&SessionId::new(ROOT));
    let (id, session) = launch(&harness).await;
    // 跑完时 b1 还在跑：停放，自己的通知现在送出，note 说它可能还会续跑。
    gates.release(&session);
    let first = next_notice(&mut root).await;
    assert_eq!(first.agent.as_str(), id);
    assert!(first.text.contains(KEEPALIVE), "{}", first.text);
    assert!(first.text.contains("<status>completed</status>"));
    assert_eq!(live(&harness).await, std::slice::from_ref(&id));
    // b1 结束：通知让子代理续跑，是续跑的第一条消息，不送顶层会话。
    env.finish(shell_notice(1, "b1"));
    harness.started(2).await;
    let prompt = resumed_with(&harness, 1);
    assert!(prompt.contains("<task-id>b1</task-id>"), "{prompt}");
    // 续跑结束：命令都交出了，结束并再送一次通知（普通的 note）。
    gates.release(&session);
    let second = next_notice(&mut root).await;
    assert_eq!(second.agent.as_str(), id, "{}", second.text);
    assert!(second.text.contains(PLAIN), "{}", second.text);
    // 续跑不属于派生它的 Agent 调用：只有第一次通知带 `<tool-use-id>`（agents.md §5）。
    assert!(first.text.contains("<tool-use-id>"), "{}", first.text);
    assert!(!second.text.contains("<tool-use-id>"), "{}", second.text);
    assert!(live(&harness).await.is_empty());
    assert_eq!(env.acked(), [1]);
}

#[tokio::test]
async fn a_command_that_ends_while_the_run_settles_still_reaches_the_subagent() {
    let gates = Gates::default();
    let env = Arc::new(NoticeEnvironment::default());
    // 第 1 次查询是建子会话时取工作目录（`inherit_directory`），第 2 次是收尾的判断：问到它还在跑，问完立即结束，通知
    // 与收尾抢同一把锁。先拿到锁的是转发 → 进邮箱，收尾带着它续跑；先拿到的是收尾 → 停放，转发让它续跑。都不丢不重。
    env.finish_after_checks(shell_notice(1, "b1"), 2);
    let harness = held_with_env(&gates, true, machine(env.clone()));
    let mut root = harness.agents.signals().signals(&SessionId::new(ROOT));
    let (id, session) = launch(&harness).await;
    gates.release(&session);
    harness.started(2).await;
    let prompt = resumed_with(&harness, 1);
    assert!(prompt.contains("<task-id>b1</task-id>"), "{prompt}");
    gates.release(&session);
    // 顶层会话只收到子代理自己的通知（停放时的一条可能有，也可能直接续跑没有），没有 b1 的。
    loop {
        let notice = next_notice(&mut root).await;
        assert_eq!(notice.agent.as_str(), id, "{}", notice.text);
        if notice.text.contains(PLAIN) {
            break;
        }
    }
    assert!(live(&harness).await.is_empty());
}

#[tokio::test]
async fn stopping_the_last_command_of_a_parked_subagent_ends_it_quietly() {
    let gates = Gates::default();
    let env = Arc::new(NoticeEnvironment::default());
    let mut harness = held_with_env(&gates, true, machine(env.clone()));
    harness.set_machine(machine(env.clone()));
    let mut root = harness.agents.signals().signals(&SessionId::new(ROOT));
    let (id, session) = launch(&harness).await;
    env.run_in(SessionId::new(&session), shell_notice(1, "b1"));
    gates.release(&session);
    let first = next_notice(&mut root).await;
    assert!(first.text.contains(KEEPALIVE), "{}", first.text);
    // 主线程停掉 b1：停掉的命令没有通知，子代理不会再醒，就此结束；通知已经送过，不再送。
    let stopped = harness
        .call(ROOT, "TaskStop", json!({"task_id": "b1"}))
        .await;
    assert!(
        text(&stopped).contains("Successfully stopped task: b1"),
        "{}",
        text(&stopped)
    );
    assert!(live(&harness).await.is_empty(), "{id} still parked");
    assert_eq!(harness.seen().len(), 1);
    let quiet = tokio::time::timeout(std::time::Duration::from_millis(50), next_notice(&mut root));
    assert!(quiet.await.is_err(), "no second notice");
}

#[tokio::test]
async fn a_command_notice_taken_during_the_run_does_not_hold_the_subagent() {
    let gates = Gates::default();
    let env = Arc::new(NoticeEnvironment::default());
    // 建子会话时的查询之后就结束：通知在运行中进邮箱，在工具边界并入。收尾时它已经交给子代理（ack 前后都一样），不停放。
    env.finish_after_checks(shell_notice(1, "b1"), 1);
    let harness = held_with_env(&gates, true, machine(env.clone()));
    let mut root = harness.agents.signals().signals(&SessionId::new(ROOT));
    let (id, session) = launch(&harness).await;
    gates.release(&session);
    let notice = next_notice(&mut root).await;
    assert_eq!(notice.agent.as_str(), id, "{}", notice.text);
    assert!(notice.text.contains(PLAIN), "{}", notice.text);
    assert_eq!(harness.absorbed.lock().unwrap().len(), 1);
    assert_eq!(harness.seen().len(), 1);
    assert!(live(&harness).await.is_empty());
}

#[tokio::test]
async fn a_foreground_subagent_does_not_wait_for_its_command() {
    let gates = Gates::default();
    let env = Arc::new(NoticeEnvironment::default());
    env.finish_after_checks(shell_notice(1, "b1"), usize::MAX);
    let harness = held_with_env(&gates, true, machine(env.clone()));
    let mut root = harness.agents.signals().signals(&SessionId::new(ROOT));
    let call = harness.call(
        ROOT,
        "Agent",
        json!({"description": "Builder", "prompt": "Build.", "run_in_background": false}),
    );
    let drive = async {
        let first = harness.started(1).await;
        gates.release(first[0].request.session.as_str());
    };
    let (output, ()) = tokio::join!(call, drive);
    // 父调用同步拿到结果；子代理结束，命令的通知照旧送顶层会话。
    assert!(
        text(&output).contains("Hello from the subagent."),
        "{}",
        text(&output)
    );
    assert!(live(&harness).await.is_empty());
    env.finish(shell_notice(1, "b1"));
    let late = next_notice(&mut root).await;
    assert_eq!(late.agent.as_str(), "b1");
}
