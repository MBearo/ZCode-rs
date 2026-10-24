//! 子代理里的 Monitor（agents.md §5.3）：事件进在跑的子代理的邮箱，终态通知带剩下的行、送出之后才 ack；交互形态下
//! 子代理跑完时有在跑的 monitor 就停放，monitor 结束后结束停放、送出自己的通知。执行环境是替身：输出流与终态通知由
//! 测试推入。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::agents::AgentSignal;
use rust_cli_kernel_api::env::{
    CommandEvent, EnvError, EnvFacts, EnvPath, FileContent, OperationContext, Outcome,
    OutputStream, ReadFile, StartCommand, WorkspaceRef, WriteFile, WriteReceipt,
};
use rust_cli_kernel_api::env_shell::{
    BackgroundInfo, BackgroundKind, BackgroundStatus, BackgroundStop, BackgroundTask, ShellNotice,
    ShellState,
};
use rust_cli_kernel_api::ids::{AgentId, OperationId, SessionId, ShellTaskId, ToolUseId};
use rust_cli_kernel_api::local_services::{Fetch, Git};
use rust_cli_kernel_api::machine::{
    Background, Checkpoints, Commands, EnvInfo, Files, Pdf, Search, Shell,
};
use rust_cli_kernel_api::monitor::{MonitorArmed, MonitorSource};
use rust_cli_kernel_api::testing::{NoticeEnvironment, shell_notice};
use serde_json::json;
use tokio::sync::mpsc;

use super::*;
use crate::test_support::{Gates, held_with_clock, text};

const ROOT: &str = "sess_parent";

/// 走着的时钟（monitor 的批与到期按时钟算）。
struct Ticking(tokio::time::Instant);

impl rust_cli_kernel_api::clock::Clock for Ticking {
    fn now_ms(&self) -> u64 {
        1_000 + u64::try_from(self.0.elapsed().as_millis()).unwrap()
    }
    fn time_zone(&self) -> String {
        "UTC".to_owned()
    }
}

fn held_ticking<E: rust_cli_kernel_api::testing::AllPorts + 'static>(
    gates: &Gates,
    keep_alive: bool,
    env: Arc<E>,
) -> crate::test_support::Harness {
    held_with_clock(
        gates,
        keep_alive,
        env,
        Arc::new(Ticking(tokio::time::Instant::now())),
    )
}

type Events = mpsc::UnboundedSender<Result<CommandEvent, EnvError>>;

/// 执行环境替身：每个操作一条可推入的输出流；终态通知经 [`NoticeEnvironment`]（子会话共用一条）。
#[derive(Default)]
struct MonitorEnv {
    notices: NoticeEnvironment,
    streams: Mutex<HashMap<OperationId, mpsc::UnboundedReceiver<Result<CommandEvent, EnvError>>>>,
    running: Mutex<HashSet<ShellTaskId>>,
    ended: Mutex<HashMap<ShellTaskId, u64>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap()
}

impl MonitorEnv {
    fn command(&self, operation: &str, task: &str) -> Events {
        let (sender, receiver) = mpsc::unbounded_channel();
        lock(&self.streams).insert(OperationId::new(operation), receiver);
        lock(&self.running).insert(ShellTaskId::new(task));
        sender
    }

    /// 命令自己结束：先记通知，再报告结束（同 env-local）。
    fn end(&self, notice: ShellNotice) {
        lock(&self.running).remove(&notice.task);
        lock(&self.ended).insert(notice.task.clone(), notice.seq);
        self.notices.finish(notice);
    }

    /// 被停掉：不记通知。
    fn stop(&self, task: &str) {
        lock(&self.running).remove(&ShellTaskId::new(task));
    }
}

fn unused() -> EnvError {
    EnvError::Unsupported {
        what: "not in this test".to_owned(),
    }
}

#[async_trait]
impl EnvInfo for MonitorEnv {
    async fn facts(&self, _w: &WorkspaceRef) -> Result<EnvFacts, EnvError> {
        Err(unused())
    }
}

#[async_trait]
impl Files for MonitorEnv {
    async fn read_file(&self, _w: &WorkspaceRef, _r: ReadFile) -> Result<FileContent, EnvError> {
        Err(unused())
    }

    async fn write_file(
        &self,
        _w: &WorkspaceRef,
        _c: OperationContext,
        _r: WriteFile,
    ) -> Result<WriteReceipt, EnvError> {
        Err(unused())
    }
}

#[async_trait]
impl Commands for MonitorEnv {
    async fn start_command(
        &self,
        _w: &WorkspaceRef,
        _c: OperationContext,
        _r: StartCommand,
    ) -> Result<(), EnvError> {
        Err(unused())
    }

    fn watch_command(
        &self,
        operation: OperationId,
        _cursor: u64,
    ) -> BoxStream<'static, Result<CommandEvent, EnvError>> {
        match lock(&self.streams).remove(&operation) {
            Some(mut receiver) => stream::poll_fn(move |cx| receiver.poll_recv(cx)).boxed(),
            None => stream::pending().boxed(),
        }
    }

    async fn cancel(&self, _operation: OperationId) -> Result<(), EnvError> {
        Err(unused())
    }

    async fn operation_status(&self, _operation: OperationId) -> Result<Outcome, EnvError> {
        Err(unused())
    }

    async fn close_session(&self, _s: &SessionId, _forget: bool) -> Result<(), EnvError> {
        Ok(())
    }
}

#[async_trait]
impl Shell for MonitorEnv {
    async fn shell_state(&self, w: &WorkspaceRef, s: &SessionId) -> Result<ShellState, EnvError> {
        let mut state = self.notices.shell_state(w, s).await?;
        state.background.extend(lock(&self.running).iter().cloned());
        Ok(state)
    }

    fn shell_notices(
        &self,
        session: &SessionId,
        after: u64,
    ) -> BoxStream<'static, Result<ShellNotice, EnvError>> {
        self.notices.shell_notices(session, after)
    }

    async fn ack_shell_notice(&self, session: &SessionId, seq: u64) -> Result<(), EnvError> {
        self.notices.ack_shell_notice(session, seq).await
    }
}

#[async_trait]
impl Background for MonitorEnv {
    async fn background_command(
        &self,
        request: BackgroundTask,
    ) -> Result<Option<BackgroundInfo>, EnvError> {
        let status = if lock(&self.running).contains(&request.task) {
            BackgroundStatus::Running
        } else if lock(&self.ended)
            .get(&request.task)
            .is_some_and(|seq| !self.notices.acked().contains(seq))
        {
            BackgroundStatus::Completed
        } else {
            return Ok(None);
        };
        Ok(Some(BackgroundInfo {
            command: "watch".to_owned(),
            status,
        }))
    }

    async fn stop_background_command(
        &self,
        _c: OperationContext,
        request: BackgroundTask,
    ) -> Result<BackgroundStop, EnvError> {
        lock(&self.running).remove(&request.task);
        Ok(BackgroundStop::NotFound)
    }
}

impl Search for MonitorEnv {}
impl Checkpoints for MonitorEnv {}
impl Pdf for MonitorEnv {}
impl Git for MonitorEnv {}
impl Fetch for MonitorEnv {}

fn armed(task: &str) -> MonitorArmed {
    MonitorArmed {
        machine: None,
        operation: OperationId::new("op_m"),
        task: ShellTaskId::new(task),
        tool_use: ToolUseId::new("toolu_m"),
        description: "errors".to_owned(),
        timeout_ms: 60_000,
        source: MonitorSource::Command,
        output: EnvPath::new("/t/b1.output"),
    }
}

/// 起一个后台子代理（在跑，被闸门挡着），返回它的 ID 与子会话。
async fn launched(harness: &crate::test_support::Harness) -> (AgentId, String) {
    let output = text(
        &harness
            .call(
                ROOT,
                "Agent",
                json!({"description": "Watcher", "prompt": "Go.", "run_in_background": true}),
            )
            .await,
    );
    let id = output
        .split("agentId: ")
        .nth(1)
        .and_then(|rest| rest.split(' ').next())
        .unwrap()
        .to_owned();
    harness.started(1).await;
    (AgentId::new(&id), format!("sess_subagent_{id}"))
}

async fn until(done: impl Fn() -> bool) {
    for _ in 0..200 {
        if done() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("timed out");
}

#[tokio::test]
async fn events_reach_the_running_subagent_and_the_end_carries_the_rest() {
    let gates = Gates::default();
    let env = Arc::new(MonitorEnv::default());
    let harness = held_ticking(&gates, false, env.clone());
    let (agent, session) = launched(&harness).await;
    let output = env.command("op_m", "b1");
    arm(&harness.agents.core, &agent, armed("b1")).unwrap();
    output
        .send(Ok(CommandEvent::Output {
            stream: OutputStream::Stdout,
            data: b"one\npart".to_vec(),
            cursor: 8,
        }))
        .unwrap();
    // 批到点之后进子代理的邮箱。
    let core = harness.agents.core.clone();
    let agent_id = agent.clone();
    until(|| {
        core.live.get(&agent_id).is_some_and(|entry| {
            matches!(&entry.state, crate::live::AgentState::Running { mailbox, .. } if !mailbox.is_empty())
        })
    })
    .await;
    let mut notice = shell_notice(1, "b1");
    notice.kind = BackgroundKind::Monitor;
    env.end(notice);
    output
        .send(Ok(CommandEvent::Finished {
            outcome: Outcome::Completed,
            exit_code: Some(0),
            timed_out: false,
            cwd_reset: None,
            output_bytes: 8,
            output_limit: None,
        }))
        .unwrap();
    // 终态送出之后才 ack。
    let notices = env.clone();
    until(|| notices.notices.acked() == [1]).await;
    assert!(!harness.agents.core.monitors.owns(&agent));
    gates.release(&session);
    let core = harness.agents.core.clone();
    until(|| {
        core.live
            .get(&agent)
            .is_some_and(|entry| matches!(entry.state, crate::live::AgentState::Finished(_)))
    })
    .await;
    let absorbed = harness.absorbed.lock().unwrap().clone();
    let all: String = absorbed.iter().map(|(_, text)| text.as_str()).collect();
    assert!(
        all.contains("<summary>Monitor event: \"errors\"</summary>\n<event>one</event>"),
        "{absorbed:?}"
    );
    assert!(
        all.contains("<task-id>b1</task-id> end Some(\"part\")"),
        "{absorbed:?}"
    );
}

#[tokio::test]
async fn a_live_monitor_parks_the_subagent_until_it_ends() {
    let gates = Gates::default();
    let env = Arc::new(MonitorEnv::default());
    let harness = held_ticking(&gates, true, env.clone());
    let signals = harness.agents.signals();
    let mut root = signals.signals(&SessionId::new(ROOT));
    let (agent, session) = launched(&harness).await;
    let output = env.command("op_m", "b1");
    arm(&harness.agents.core, &agent, armed("b1")).unwrap();
    gates.release(&session);
    let core = harness.agents.core.clone();
    let agent_id = agent.clone();
    until(|| {
        core.live
            .get(&agent_id)
            .is_some_and(|entry| matches!(entry.state, crate::live::AgentState::Parked(_)))
    })
    .await;
    // 停掉（没有终态通知、没有剩下的行）：最后一个 monitor 结束，子代理结束停放、送出自己的通知。
    env.stop("b1");
    output
        .send(Ok(CommandEvent::Finished {
            outcome: Outcome::Cancelled,
            exit_code: Some(137),
            timed_out: false,
            cwd_reset: None,
            output_bytes: 0,
            output_limit: None,
        }))
        .unwrap();
    let Some(AgentSignal::Notice(own)) = root.next().await else {
        panic!("expected the agent's own notice");
    };
    assert_eq!(own.agent, agent);
    assert!(!harness.agents.core.monitors.owns(&agent));
}
