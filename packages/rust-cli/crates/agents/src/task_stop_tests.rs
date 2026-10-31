//! TaskStop 的结果与校验（后台命令、后台子代理；agents.md §6）。
//! 读对照数据的两项在 `task_stop_parity_tests.rs`（docs/specs/rust-cli/export.md §5）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rust_cli_kernel_api::agents::AgentSignal;
use rust_cli_kernel_api::env::{
    CommandEvent, EnvError, EnvFacts, FileContent, OperationContext, ReadFile, StartCommand,
    WorkspaceRef, WriteFile, WriteReceipt,
};
use rust_cli_kernel_api::env_shell::{
    BackgroundInfo, BackgroundStatus, BackgroundStop, BackgroundTask,
};
use rust_cli_kernel_api::ids::{OperationId, SessionId};
use rust_cli_kernel_api::local_services::{Fetch, Git};
use rust_cli_kernel_api::machine::{
    Background, Checkpoints, Commands, EnvInfo, Files, Pdf, Search, Shell,
};
use serde_json::json;
use tokio::sync::Notify;

use crate::test_support::{harness, text};

fn unsupported() -> EnvError {
    EnvError::Unsupported {
        what: "test".to_owned(),
    }
}

/// 执行环境的后台命令账本：(会话, 任务) → (命令, 状态)。停止的规则同 env-local（§2.6）。
#[derive(Default)]
struct Shells {
    commands: Mutex<HashMap<(String, String), (String, BackgroundStatus)>>,
    stops: Mutex<Vec<OperationId>>,
}

impl Shells {
    fn with(session: &str, task: &str, command: &str, status: BackgroundStatus) -> Self {
        let shells = Self::default();
        shells.commands.lock().unwrap().insert(
            (session.to_owned(), task.to_owned()),
            (command.to_owned(), status),
        );
        shells
    }

    fn key(request: &BackgroundTask) -> (String, String) {
        (request.session.to_string(), request.task.to_string())
    }
}

#[async_trait]
impl EnvInfo for Shells {
    async fn facts(&self, _workspace: &WorkspaceRef) -> Result<EnvFacts, EnvError> {
        Err(unsupported())
    }
}

#[async_trait]
impl Files for Shells {
    async fn read_file(&self, _: &WorkspaceRef, _: ReadFile) -> Result<FileContent, EnvError> {
        Err(unsupported())
    }

    async fn write_file(
        &self,
        _: &WorkspaceRef,
        _: OperationContext,
        _: WriteFile,
    ) -> Result<WriteReceipt, EnvError> {
        Err(unsupported())
    }
}

#[async_trait]
impl Commands for Shells {
    async fn start_command(
        &self,
        _: &WorkspaceRef,
        _: OperationContext,
        _: StartCommand,
    ) -> Result<BoxStream<'static, Result<CommandEvent, EnvError>>, EnvError> {
        Err(unsupported())
    }

    fn watch_command(
        &self,
        _: OperationId,
        _: u64,
    ) -> BoxStream<'static, Result<CommandEvent, EnvError>> {
        futures_util::stream::iter([Err(unsupported())]).boxed()
    }

    async fn cancel(&self, _: OperationId) -> Result<(), EnvError> {
        Err(unsupported())
    }

    async fn close_session(&self, _: &SessionId, _: bool) -> Result<(), EnvError> {
        Ok(())
    }
}

#[async_trait]
impl Shell for Shells {
    async fn shell_state(
        &self,
        workspace: &WorkspaceRef,
        session: &SessionId,
    ) -> Result<rust_cli_kernel_api::env_shell::ShellState, EnvError> {
        let commands = self.commands.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(rust_cli_kernel_api::env_shell::ShellState {
            cwd: workspace.root.clone(),
            outputs: None,
            background: commands
                .keys()
                .filter(|(owner, _)| owner == session.as_str())
                .map(|(_, task)| rust_cli_kernel_api::ids::ShellTaskId::new(task.as_str()))
                .collect(),
        })
    }
}

#[async_trait]
impl Background for Shells {
    async fn background_command(
        &self,
        request: BackgroundTask,
    ) -> Result<Option<BackgroundInfo>, EnvError> {
        let commands = self.commands.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(commands
            .get(&Self::key(&request))
            .map(|(command, status)| BackgroundInfo {
                command: command.clone(),
                status: *status,
                kind: Default::default(),
                description: String::new(),
            }))
    }

    async fn stop_background_command(
        &self,
        context: OperationContext,
        request: BackgroundTask,
    ) -> Result<BackgroundStop, EnvError> {
        self.stops.lock().unwrap().push(context.operation);
        let mut commands = self.commands.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(match commands.get(&Self::key(&request)).cloned() {
            None => BackgroundStop::NotFound,
            Some((command, BackgroundStatus::Running)) => {
                commands.remove(&Self::key(&request));
                BackgroundStop::Stopped { command }
            }
            Some((_, status)) => BackgroundStop::NotRunning { status },
        })
    }
}

impl Search for Shells {}
impl Checkpoints for Shells {}
impl Pdf for Shells {}
impl Git for Shells {}
impl Fetch for Shells {}

#[tokio::test]
async fn an_ended_command_is_not_running() {
    let mut harness = harness(None);
    harness.set_env(Arc::new(Shells::with(
        "sess_parent",
        "b1",
        "exit 3",
        BackgroundStatus::Failed,
    )));
    let output = harness
        .call("sess_parent", "KillShell", json!({"shell_id": "b1"}))
        .await;
    assert!(output.is_error);
    assert_eq!(
        text(&output),
        "<tool_use_error>Task b1 is not running (status: failed)</tool_use_error>"
    );
}

/// 找不到时列出在跑的后台子代理。
#[tokio::test]
async fn not_found_lists_the_running_background_agents() {
    let harness = harness(Some(Arc::new(Notify::new())));
    harness
        .call(
            "sess_parent",
            "Agent",
            json!({"description": "Long  job\n", "prompt": "Work."}),
        )
        .await;
    harness.started(1).await;
    let output = harness
        .call("sess_parent", "TaskStop", json!({"task_id": "nope"}))
        .await;
    assert_eq!(
        text(&output),
        "<tool_use_error>No task found with ID: nope. Running background agents: agent_1 (Long job)</tool_use_error>"
    );
}

/// L18（主线程能停任何任务）：主线程停子代理启动的后台命令，按那个子会话停；子代理只找自己的。
#[tokio::test]
async fn the_main_thread_stops_a_command_of_its_subagent() {
    let mut harness = harness(Some(Arc::new(Notify::new())));
    harness
        .call(
            "sess_parent",
            "Agent",
            json!({"description": "Shell owner", "prompt": "Start a shell."}),
        )
        .await;
    harness.started(1).await;
    let shells = Arc::new(Shells::with(
        "sess_subagent_agent_1",
        "b1",
        "sleep 30",
        BackgroundStatus::Running,
    ));
    harness.set_env(shells.clone());
    let stopped = harness
        .call("sess_parent", "TaskStop", json!({"task_id": "b1"}))
        .await;
    assert!(!stopped.is_error, "{}", text(&stopped));
    assert_eq!(
        text(&stopped),
        r#"{"message":"Successfully stopped task: b1 (sleep 30)","task_id":"b1","task_type":"local_bash","command":"sleep 30"}"#
    );
    assert!(shells.commands.lock().unwrap().is_empty());
    // 另一个会话（不在这个顶层会话里）看不到它。
    harness.set_env(Arc::new(Shells::with(
        "sess_subagent_agent_1",
        "b2",
        "sleep 30",
        BackgroundStatus::Running,
    )));
    let other = harness
        .call("sess_other", "TaskStop", json!({"task_id": "b2"}))
        .await;
    assert!(other.is_error);
    assert!(
        text(&other).contains("No task found with ID: b2"),
        "{}",
        text(&other)
    );
}

/// L18（停放的子代理被停止）：停止停放的子代理时，停掉它子会话里还在跑的后台命令；它派生
/// 的后台子代理不停。
#[tokio::test]
async fn stopping_a_parked_agent_stops_its_commands_but_not_its_children() {
    let gates = crate::test_support::Gates::default();
    let mut harness = crate::test_support::held(&gates, true);
    let id_of = |o: &str| {
        o.lines()
            .nth(1)
            .and_then(|l| l.split(' ').nth(1))
            .unwrap()
            .to_owned()
    };
    let outer = id_of(&text(
        &harness
            .call(
                "sess_parent",
                "Agent",
                json!({"description": "Outer", "prompt": "Go."}),
            )
            .await,
    ));
    let outer_session = format!("sess_subagent_{outer}");
    harness.started(1).await;
    let inner = id_of(&text(
        &harness
            .call(
                &outer_session,
                "Agent",
                json!({"description": "Inner", "prompt": "Dig."}),
            )
            .await,
    ));
    harness.started(2).await;
    gates.release(&outer_session); // P 跑完、C 还在跑：P 停放。
    for _ in 0..10_000 {
        let listed = text(&harness.call("sess_parent", "ListAgents", json!({})).await);
        if listed.contains(&format!("{outer}  ·  general-purpose  ·  completed")) {
            break;
        }
        tokio::task::yield_now().await;
    }
    let shells = Arc::new(Shells::with(
        &outer_session,
        "b1",
        "sleep 30",
        BackgroundStatus::Running,
    ));
    harness.set_env(shells.clone());
    let stopped = harness
        .call("sess_parent", "TaskStop", json!({"task_id": outer}))
        .await;
    assert!(!stopped.is_error, "{}", text(&stopped));
    assert!(shells.commands.lock().unwrap().is_empty());
    assert_eq!(shells.stops.lock().unwrap().len(), 1);
    let background = harness
        .agents
        .signals()
        .background(&SessionId::new("sess_parent"))
        .await;
    assert!(
        background.iter().any(|agent| agent.as_str() == inner),
        "{background:?}"
    );
}

/// 用户在 `/tasks` 里停止（tui-display.md §3.1）：列出后台子代理，停止走同一条路径（通知同样是 killed），
/// 再停一次按 TaskStop 的文字拒绝。
#[tokio::test]
async fn the_user_stops_a_background_agent_through_the_same_path() {
    let harness = harness(Some(Arc::new(Notify::new())));
    let parent = SessionId::new("sess_parent");
    let signals = harness.agents.signals();
    let mut stream = signals.signals(&parent);
    harness
        .call(
            "sess_parent",
            "Agent",
            json!({"description": "Wait for me", "prompt": "Say hello slowly."}),
        )
        .await;
    harness.started(1).await;
    let entries = signals.background_entries(&parent).await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, "agent_1");
    assert_eq!(entries[0].description, "Wait for me");
    let operation = || OperationContext {
        environment: rust_cli_kernel_api::ids::EnvironmentId::new("local"),
        generation: rust_cli_kernel_api::ids::Generation::new("g1"),
        owner_epoch: rust_cli_kernel_api::ids::OwnerEpoch::LOCAL,
        operation: OperationId::new("op_user"),
        origin: rust_cli_kernel_api::env::Origin::User {
            session: Some(parent.clone()),
        },
    };
    let stopped = signals.stop_task(&parent, "agent_1", operation()).await;
    assert_eq!(
        stopped.as_deref(),
        Ok("Successfully stopped task: agent_1 (Wait for me)")
    );
    let Some(AgentSignal::Notice(notice)) = stream.next().await else {
        panic!("expected a notice");
    };
    assert_eq!(notice.status, "killed");
    let again = signals.stop_task(&parent, "agent_1", operation()).await;
    assert_eq!(
        again,
        Err("Task agent_1 is not running (status: killed)".to_owned())
    );
    assert!(signals.background_entries(&parent).await.is_empty());
}

#[cfg(test)]
#[cfg(feature = "parity")]
#[rustfmt::skip]
#[path = "task_stop_parity_tests.rs"]
mod parity_tests;

/// 远端机器上的后台命令（R3，remote-exec.md §20.7.2）：TaskStop 依次问本机与连上的机器，在任务所在的那台上停。
#[tokio::test]
async fn a_command_on_a_remote_machine_is_stopped_there() {
    let remote = Arc::new(Shells::with(
        "sess_1",
        "b0000000r",
        "sleep 100",
        BackgroundStatus::Running,
    ));
    let registry = rust_cli_kernel_api::testing::TestMachines::new(Arc::new(
        rust_cli_kernel_api::testing::UnusedEnvironment,
    ));
    registry.connect("dev-a", remote.clone(), "/home/dev/proj");
    let harness = crate::test_support::with_machines(registry);
    let output = harness
        .call("sess_1", "TaskStop", json!({"task_id": "b0000000r"}))
        .await;
    assert!(!output.is_error, "{}", text(&output));
    assert!(text(&output).contains("Successfully stopped task: b0000000r (sleep 100)"));
    assert_eq!(remote.stops.lock().unwrap().len(), 1);
}

/// F98（remote-exec.md §25.2.4）：停的是远端机器上的命令时，回执在 `command` 之后带 `machine`，display 也带（TUI 的工具行
/// 接 `@dev-a`）；`task_type` 照旧。本机的不变。旧代码的回执与 display 都没有机器。
#[tokio::test]
async fn stopping_a_remote_command_names_its_machine() {
    let remote = Arc::new(Shells::with(
        "sess_1",
        "b0000000r",
        "sleep 100",
        BackgroundStatus::Running,
    ));
    let registry = rust_cli_kernel_api::testing::TestMachines::new(Arc::new(
        rust_cli_kernel_api::testing::UnusedEnvironment,
    ));
    registry.connect("dev-a", remote, "/home/dev/proj");
    let on_dev_a = crate::test_support::with_machines(registry);
    let output = on_dev_a
        .call("sess_1", "TaskStop", json!({"task_id": "b0000000r"}))
        .await;
    assert_eq!(
        text(&output),
        r#"{"message":"Successfully stopped task: b0000000r (sleep 100)","task_id":"b0000000r","task_type":"local_bash","command":"sleep 100","machine":"dev-a"}"#
    );
    assert_eq!(output.display, json!({"machine": "dev-a"}));

    let mut local = harness(None);
    local.set_env(Arc::new(Shells::with(
        "sess_1",
        "b0000000l",
        "sleep 100",
        BackgroundStatus::Running,
    )));
    let output = local
        .call("sess_1", "TaskStop", json!({"task_id": "b0000000l"}))
        .await;
    assert_eq!(
        text(&output),
        r#"{"message":"Successfully stopped task: b0000000l (sleep 100)","task_id":"b0000000l","task_type":"local_bash","command":"sleep 100"}"#
    );
    assert_eq!(output.display, serde_json::Value::Null);
}
