//! 后台任务的查看与停止（tui-display.md §3.1）：命令来自执行环境的账本（带输出末尾），子代理来自 agents；
//! `StopTask` 交给 agents 的停止路径，回执带它的文字。修复线 F34：子代理列表与后台任务的同一份快照。

use std::sync::Mutex;

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::agents::{AgentSignal, AgentSignals};
use rust_cli_kernel_api::env::{
    CommandEvent, EnvError, EnvFacts, EnvPath, FileContent, OperationContext, Outcome, ReadFile,
    StartCommand, WorkspaceRef, WriteFile, WriteReceipt,
};
use rust_cli_kernel_api::env_files::{ReadText, TextRead, TextSlice};
use rust_cli_kernel_api::env_shell::{
    BackgroundInfo, BackgroundStatus, BackgroundTask, ShellState,
};
use rust_cli_kernel_api::ids::{AgentId, OperationId, SessionId, ShellTaskId};
use rust_cli_kernel_api::local_services::{Fetch, Git};
use rust_cli_kernel_api::machine::{
    Background, Checkpoints, Commands, EnvInfo, Files, Pdf, Search, Shell,
};
use rust_cli_kernel_api::manage::{BackgroundEntry, BackgroundKind};
use rust_cli_kernel_api::panels::{ShellOutput, SubagentEntry, TasksSnapshot};
use rust_cli_kernel_api::session_api::{Ack, Command, SessionHandle};
use rust_cli_kernel_api::testing::ScriptedModel;

use crate::test_support::{Setup, envelope};

fn unused() -> EnvError {
    EnvError::Unsupported {
        what: "test".to_owned(),
    }
}

/// 只有两条后台命令的执行环境：`b1` 在跑（有输出文件），`b2` 已结束。
struct Shells;

#[async_trait]
impl EnvInfo for Shells {
    async fn facts(&self, _workspace: &WorkspaceRef) -> Result<EnvFacts, EnvError> {
        Err(unused())
    }
}

#[async_trait]
impl Files for Shells {
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

    async fn read_text(&self, _w: &WorkspaceRef, request: ReadText) -> Result<TextRead, EnvError> {
        // b2 的输出太大，b3 还没有输出文件（tui-panels.md §3.3）。
        match request.path.as_str() {
            "/tmp/tasks/b2.output" => return Ok(TextRead::TooLarge { size: 2 << 20 }),
            "/tmp/tasks/b3.output" => {
                return Err(EnvError::NotFound {
                    path: request.path.as_str().to_owned(),
                });
            }
            _ => {}
        }
        assert_eq!(request.path.as_str(), "/tmp/tasks/b1.output");
        let content = (1..=45)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(TextRead::Text(TextSlice {
            content,
            line_count: 45,
            total_lines: 45,
            total_bytes: 0,
            read_bytes: 0,
            mtime_ms: 0,
            version: rust_cli_kernel_api::env::FileVersion::new("v1"),
        }))
    }
}

#[async_trait]
impl Commands for Shells {
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
        _o: OperationId,
        _cursor: u64,
    ) -> BoxStream<'static, Result<CommandEvent, EnvError>> {
        stream::iter([Err(unused())]).boxed()
    }

    async fn cancel(&self, _o: OperationId) -> Result<(), EnvError> {
        Err(unused())
    }

    async fn operation_status(&self, _o: OperationId) -> Result<Outcome, EnvError> {
        Err(unused())
    }

    async fn close_session(&self, _s: &SessionId, _forget: bool) -> Result<(), EnvError> {
        Ok(())
    }
}

#[async_trait]
impl Shell for Shells {
    async fn shell_state(&self, _w: &WorkspaceRef, _s: &SessionId) -> Result<ShellState, EnvError> {
        Ok(ShellState {
            cwd: EnvPath::new("/Users/dev/project"),
            outputs: Some(EnvPath::new("/tmp/tasks")),
            background: vec![ShellTaskId::new("b1"), ShellTaskId::new("b2")],
        })
    }
}

#[async_trait]
impl Background for Shells {
    async fn background_command(
        &self,
        task: BackgroundTask,
    ) -> Result<Option<BackgroundInfo>, EnvError> {
        let status = if task.task.as_str() == "b1" {
            BackgroundStatus::Running
        } else {
            BackgroundStatus::Completed
        };
        Ok(Some(BackgroundInfo {
            command: format!("run {}", task.task),
            status,
        }))
    }

    async fn background_elapsed(&self, task: BackgroundTask) -> Result<Option<u64>, EnvError> {
        Ok((task.task.as_str() == "b1").then_some(65_000))
    }
}

impl Search for Shells {}
impl Checkpoints for Shells {}
impl Pdf for Shells {}
impl Git for Shells {}
impl Fetch for Shells {}

/// 一个后台子代理；记下停止请求。
#[derive(Default)]
struct OneAgent {
    stopped: Mutex<Vec<String>>,
}

#[async_trait]
impl AgentSignals for OneAgent {
    fn signals(&self, _session: &SessionId) -> BoxStream<'static, AgentSignal> {
        stream::pending().boxed()
    }
    async fn ack(&self, _session: &SessionId, _seq: u64) {}
    async fn background(&self, _session: &SessionId) -> Vec<AgentId> {
        Vec::new()
    }
    async fn background_entries(&self, _session: &SessionId) -> Vec<BackgroundEntry> {
        vec![BackgroundEntry {
            id: "agent_1".to_owned(),
            kind: BackgroundKind::Agent,
            description: "Review".to_owned(),
            status: BackgroundStatus::Running,
            output: None,
        }]
    }
    async fn stop_task(
        &self,
        _session: &SessionId,
        task: &str,
        _operation: OperationContext,
    ) -> Result<String, String> {
        self.stopped.lock().unwrap().push(task.to_owned());
        if task == "agent_1" {
            Ok("Successfully stopped task: agent_1 (Review)".to_owned())
        } else {
            Err(format!("No task found with ID: {task}"))
        }
    }
}

#[tokio::test]
async fn background_tasks_list_running_commands_and_agents() {
    let mut setup = Setup::new(std::sync::Arc::new(ScriptedModel::new(Vec::new())));
    setup.set_env(std::sync::Arc::new(Shells));
    setup.agents = std::sync::Arc::new(OneAgent::default());
    let built = setup.build().await;
    let entries = built.session.background_tasks().await.unwrap();
    assert_eq!(entries.len(), 2, "{entries:#?}");
    assert_eq!(entries[0].id, "b1");
    assert_eq!(entries[0].kind, BackgroundKind::Shell);
    assert_eq!(entries[0].description, "run b1");
    let output = entries[0].output.as_deref().unwrap();
    // 只留末尾 40 行。
    assert!(
        output.starts_with("line 6\n") && output.ends_with("line 45"),
        "{output}"
    );
    assert_eq!(entries[1].id, "agent_1");
}

/// agents 的那一半只经 `tasks_snapshot` 一次给出（修复线 F34）；两个旧方法给的不同，用来认出会话取的是哪一个。
struct OneRead;

#[async_trait]
impl AgentSignals for OneRead {
    fn signals(&self, _session: &SessionId) -> BoxStream<'static, AgentSignal> {
        stream::pending().boxed()
    }
    async fn ack(&self, _session: &SessionId, _seq: u64) {}
    async fn background(&self, _session: &SessionId) -> Vec<AgentId> {
        Vec::new()
    }
    async fn tasks_snapshot(&self, session: &SessionId) -> TasksSnapshot {
        let entry: SubagentEntry = serde_json::from_value(serde_json::json!({
            "agent": "agent_1", "child": "sess_subagent_agent_1", "agent_type": "general-purpose",
            "description": "Review", "status": "running", "background": true, "elapsed_ms": 1,
            "version": 0
        }))
        .unwrap();
        TasksSnapshot {
            subagents: vec![entry],
            background: OneAgent::default().background_entries(session).await,
        }
    }
}

/// 修复线 F34（tui-panels.md §0、§3.1）：一次查询给出子代理条目与后台任务；命令在前（不带输出末尾）、子代理在后。
#[tokio::test]
async fn the_tasks_snapshot_has_the_commands_and_the_agents_in_one_reply() {
    let mut setup = Setup::new(std::sync::Arc::new(ScriptedModel::new(Vec::new())));
    setup.set_env(std::sync::Arc::new(Shells));
    setup.agents = std::sync::Arc::new(OneRead);
    let built = setup.build().await;
    let snapshot = built.session.tasks_snapshot().await.unwrap();
    let rows: Vec<_> = snapshot
        .subagents
        .iter()
        .map(|e| e.agent.as_str())
        .collect();
    assert_eq!(rows, ["agent_1"]);
    let tasks: Vec<_> = snapshot
        .background
        .iter()
        .map(|e| (e.id.as_str(), e.kind, e.output.as_deref()))
        .collect();
    assert_eq!(
        tasks,
        [
            ("b1", BackgroundKind::Shell, None),
            ("agent_1", BackgroundKind::Agent, None)
        ]
    );
    // 旧的两个查询照旧（只加不改）：这个替身的旧方法没有条目。
    assert!(built.session.subagents().await.unwrap().is_empty());
}

#[tokio::test]
async fn stop_task_goes_through_the_agents_stop_path() {
    let agents = std::sync::Arc::new(OneAgent::default());
    let mut setup = Setup::new(std::sync::Arc::new(ScriptedModel::new(Vec::new())));
    setup.agents = agents.clone();
    let built = setup.build().await;
    let stop = |task: &str| Command::StopTask {
        task: task.to_owned(),
    };
    let ack = built.session.send(envelope("c1", stop("agent_1"))).await;
    assert_eq!(
        ack,
        Ack::Accepted {
            result: serde_json::json!({"message": "Successfully stopped task: agent_1 (Review)"})
        }
    );
    let ack = built.session.send(envelope("c2", stop("nope"))).await;
    assert_eq!(
        ack,
        Ack::Rejected {
            reason: "No task found with ID: nope".to_owned()
        }
    );
    assert_eq!(*agents.stopped.lock().unwrap(), ["agent_1", "nope"]);
}

#[tokio::test]
async fn shell_details_show_the_last_ten_lines_and_the_runtime() {
    let mut setup = Setup::new(std::sync::Arc::new(ScriptedModel::new(Vec::new())));
    setup.set_env(std::sync::Arc::new(Shells));
    let built = setup.build().await;
    let detail = built.session.shell_detail("b1").await.unwrap().unwrap();
    assert_eq!(detail.command, "run b1");
    assert_eq!(detail.status, BackgroundStatus::Running);
    assert_eq!(detail.elapsed_ms, Some(65_000));
    let ShellOutput::Tail { lines, total } = &detail.output else {
        panic!("{detail:?}");
    };
    assert_eq!(*total, 45);
    assert_eq!(lines.len(), 10);
    assert_eq!(
        (lines[0].as_str(), lines[9].as_str()),
        ("line 36", "line 45")
    );
    // 已结束：状态是终态，没有 Runtime；输出太大时不预览。
    let ended = built.session.shell_detail("b2").await.unwrap().unwrap();
    assert_eq!(ended.status, BackgroundStatus::Completed);
    assert_eq!(ended.elapsed_ms, None);
    assert_eq!(ended.output, ShellOutput::TooLarge { bytes: 2 << 20 });
    // 还没有输出文件：空的输出。
    let fresh = built.session.shell_detail("b3").await.unwrap().unwrap();
    assert_eq!(
        fresh.output,
        ShellOutput::Tail {
            lines: Vec::new(),
            total: 0
        }
    );
}
