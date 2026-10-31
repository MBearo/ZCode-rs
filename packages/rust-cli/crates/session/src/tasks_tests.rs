//! 后台任务的查看与停止（tui-display.md §3.1）：命令来自执行环境的账本（带输出末尾），子代理来自 agents；
//! `StopTask` 交给 agents 的停止路径，回执带它的文字。修复线 F34：子代理列表与后台任务的同一份快照。

use std::sync::Mutex;

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::agents::{AgentSignal, AgentSignals};
use rust_cli_kernel_api::env::{
    CommandEvent, EnvError, EnvFacts, EnvPath, FileContent, OperationContext, ReadFile,
    StartCommand, WorkspaceRef, WriteFile, WriteReceipt,
};
use rust_cli_kernel_api::env_files::{ReadText, TextRead, TextSlice};
use rust_cli_kernel_api::env_shell::{
    BackgroundInfo, BackgroundKind as BackgroundKind_, BackgroundStatus, BackgroundTask, ShellState,
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

/// 只有两条后台命令的执行环境：`b1` 在跑（有输出文件），`b2` 已结束。远端执行 R4：另一台机器用 [`Shells::REMOTE`]
/// （任务 `r1`、`r2`，输出在 `/srv/tasks`）；不是自己前缀的任务不认识。
struct Shells {
    prefix: &'static str,
    outputs: &'static str,
    /// 远端执行 F96：为真时 `shell_state` 与 `close_session` 一直不回（连接断了、调用在等重连）。
    hang: Option<&'static std::sync::atomic::AtomicBool>,
    /// 远端执行 F98：`b1` 的输出是 `tick-1` … `tick-12`、以换行结尾（命令的输出一般都是），`total_lines` 照 env-local 的
    /// 读法（`\n` 的个数加一）。
    ticks: bool,
    /// 修复线 F117：账本里另有 monitor（[`MONITORS`]）。
    monitors: bool,
}

/// 修复线 F117 的账本：任务 → （命令，种类，启动时的说明）。`b4` 是 description 为空的 monitor（名称退回命令原文）。
const MONITORS: [(&str, &str, BackgroundKind_, &str); 4] = [
    ("b1", "npm test", BackgroundKind_::Bash, "Run the tests"),
    (
        "b2",
        "tail -f build.log",
        BackgroundKind_::Monitor,
        "build log",
    ),
    ("b4", "tail -F app.log", BackgroundKind_::Monitor, ""),
    (
        "s3",
        "wss://events.example.com/feed",
        BackgroundKind_::Monitor,
        "deploy feed",
    ),
];

impl Shells {
    const LOCAL: Self = Self {
        prefix: "b",
        outputs: "/tmp/tasks",
        hang: None,
        ticks: false,
        monitors: false,
    };
    const REMOTE: Self = Self {
        prefix: "r",
        outputs: "/srv/tasks",
        hang: None,
        ticks: false,
        monitors: false,
    };

    async fn wait_if_hanging(&self) {
        if self
            .hang
            .is_some_and(|hang| hang.load(std::sync::atomic::Ordering::SeqCst))
        {
            std::future::pending::<()>().await;
        }
    }
}

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
        let (outputs, prefix) = (self.outputs, self.prefix);
        // b2 的输出太大，b3 还没有输出文件（tui-panels.md §3.3）。
        let path = request.path.as_str();
        if path == format!("{outputs}/{prefix}2.output") {
            return Ok(TextRead::TooLarge { size: 2 << 20 });
        }
        if path == format!("{outputs}/{prefix}3.output") {
            return Err(EnvError::NotFound {
                path: path.to_owned(),
            });
        }
        assert_eq!(path, format!("{outputs}/{prefix}1.output"));
        if self.ticks {
            let content: String = (1..=12).map(|n| format!("tick-{n}\n")).collect();
            let lines = content.matches('\n').count() as u64 + 1;
            return Ok(TextRead::Text(TextSlice {
                content,
                line_count: lines,
                total_lines: lines,
                total_bytes: 0,
                read_bytes: 0,
                mtime_ms: 0,
                version: rust_cli_kernel_api::env::FileVersion::new("v1"),
            }));
        }
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
    ) -> Result<BoxStream<'static, Result<CommandEvent, EnvError>>, EnvError> {
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

    async fn close_session(&self, _s: &SessionId, _forget: bool) -> Result<(), EnvError> {
        self.wait_if_hanging().await;
        Ok(())
    }
}

#[async_trait]
impl Shell for Shells {
    async fn shell_state(&self, _w: &WorkspaceRef, _s: &SessionId) -> Result<ShellState, EnvError> {
        self.wait_if_hanging().await;
        let prefix = self.prefix;
        let background = if self.monitors {
            MONITORS
                .iter()
                .map(|(task, ..)| ShellTaskId::new(*task))
                .collect()
        } else {
            vec![
                ShellTaskId::new(format!("{prefix}1")),
                ShellTaskId::new(format!("{prefix}2")),
            ]
        };
        Ok(ShellState {
            cwd: EnvPath::new("/Users/dev/project"),
            outputs: Some(EnvPath::new(self.outputs)),
            background,
        })
    }
}

#[async_trait]
impl Background for Shells {
    async fn background_command(
        &self,
        task: BackgroundTask,
    ) -> Result<Option<BackgroundInfo>, EnvError> {
        if self.monitors {
            return Ok(MONITORS
                .iter()
                .find(|(id, ..)| *id == task.task.as_str())
                .map(|(_, command, kind, description)| BackgroundInfo {
                    command: (*command).to_owned(),
                    status: BackgroundStatus::Running,
                    kind: *kind,
                    description: (*description).to_owned(),
                }));
        }
        if !task.task.as_str().starts_with(self.prefix) {
            return Ok(None);
        }
        let status = if task.task.as_str() == format!("{}1", self.prefix) {
            BackgroundStatus::Running
        } else {
            BackgroundStatus::Completed
        };
        Ok(Some(BackgroundInfo {
            command: format!("run {}", task.task),
            status,
            kind: Default::default(),
            description: String::new(),
        }))
    }

    async fn background_elapsed(&self, task: BackgroundTask) -> Result<Option<u64>, EnvError> {
        Ok((task.task.as_str() == format!("{}1", self.prefix)).then_some(65_000))
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
            machine: None,
            reconnecting: false,
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
    setup.set_env(std::sync::Arc::new(Shells::LOCAL));
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
    setup.set_env(std::sync::Arc::new(Shells::LOCAL));
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

/// 修复线 F117（tui-display.md §2.3、tui-panels.md §3.2）：执行环境报的种类照抄进快照——Bash 是 `Shell`，Monitor（命令
/// 来源与 WebSocket 来源）是 `Monitor`；monitor 的名称是启动时的 description（空时是命令原文），shell 的仍是命令原文。
/// 修前全是 `Shell`、名称全是命令原文。
#[tokio::test]
async fn monitors_keep_their_kind_and_description_in_the_snapshot() {
    let mut setup = Setup::new(std::sync::Arc::new(ScriptedModel::new(Vec::new())));
    setup.set_env(std::sync::Arc::new(Shells {
        monitors: true,
        ..Shells::LOCAL
    }));
    setup.agents = std::sync::Arc::new(OneRead);
    let built = setup.build().await;
    let snapshot = built.session.tasks_snapshot().await.unwrap();
    let tasks: Vec<_> = snapshot
        .background
        .iter()
        .map(|e| (e.id.as_str(), e.kind, e.description.as_str()))
        .collect();
    assert_eq!(
        tasks,
        [
            ("b1", BackgroundKind::Shell, "npm test"),
            ("b2", BackgroundKind::Monitor, "build log"),
            ("b4", BackgroundKind::Monitor, "tail -F app.log"),
            ("s3", BackgroundKind::Monitor, "deploy feed"),
            ("agent_1", BackgroundKind::Agent, "Review"),
        ]
    );
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
    setup.set_env(std::sync::Arc::new(Shells::LOCAL));
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

/// 远端执行 F98（remote-exec.md §25.4.6，Docker 试用 L11）：输出文件以换行结尾时，总行数不把最后的换行算一行，与框里
/// 的行对得上。旧代码按 `\n` 的个数加一，框里是 `tick-3 … tick-12`、下面写 13 行。
#[tokio::test]
async fn the_line_count_of_an_output_that_ends_with_a_newline_matches_the_lines() {
    let mut setup = Setup::new(std::sync::Arc::new(ScriptedModel::new(Vec::new())));
    setup.set_env(std::sync::Arc::new(Shells {
        ticks: true,
        ..Shells::LOCAL
    }));
    let built = setup.build().await;
    let detail = built.session.shell_detail("b1").await.unwrap().unwrap();
    let ShellOutput::Tail { lines, total } = &detail.output else {
        panic!("{detail:?}");
    };
    assert_eq!(
        (
            lines.first().map(String::as_str),
            lines.last().map(String::as_str),
            lines.len()
        ),
        (Some("tick-3"), Some("tick-12"), 10)
    );
    assert_eq!(*total, 12);
}

/// 远端执行 R4（remote-exec.md §21.6）：后台任务的快照也列连上的远端机器上在跑的命令（带 `machine`）；Shell 详情按
/// 任务 ID 依次问本机与远端，在哪台找到就读那台的输出文件。
#[tokio::test]
async fn remote_background_commands_are_listed_with_their_machine() {
    use rust_cli_kernel_api::testing::TestMachines;
    let machines = TestMachines::new(std::sync::Arc::new(Shells::LOCAL));
    machines.connect("dev-a", std::sync::Arc::new(Shells::REMOTE), "/srv/proj");
    let mut setup = Setup::new(std::sync::Arc::new(ScriptedModel::new(Vec::new())));
    setup.set_env(std::sync::Arc::new(Shells::LOCAL));
    setup.machines = Some(machines);
    setup.agents = std::sync::Arc::new(OneAgent::default());
    let built = setup.build().await;
    let entries = built.session.background_tasks().await.unwrap();
    let rows: Vec<_> = entries
        .iter()
        .map(|e| (e.id.as_str(), e.machine.as_deref()))
        .collect();
    assert_eq!(
        rows,
        [("b1", None), ("r1", Some("dev-a")), ("agent_1", None)]
    );
    // 远端的输出末尾读那台机器的文件。
    assert!(entries[1].output.as_deref().unwrap().ends_with("line 45"));
    let snapshot = built.session.tasks_snapshot().await.unwrap();
    let rows: Vec<_> = snapshot
        .background
        .iter()
        .map(|e| (e.id.as_str(), e.machine.as_deref(), e.output.is_none()))
        .collect();
    assert_eq!(
        rows,
        [
            ("b1", None, true),
            ("r1", Some("dev-a"), true),
            ("agent_1", None, true)
        ]
    );
    let detail = built.session.shell_detail("r1").await.unwrap().unwrap();
    assert_eq!(detail.command, "run r1");
    assert_eq!(detail.status, BackgroundStatus::Running);
    assert_eq!(detail.elapsed_ms, Some(65_000));
    assert!(matches!(detail.output, ShellOutput::Tail { total: 45, .. }));
    assert!(built.session.shell_detail("x9").await.unwrap().is_none());
}

/// 远端执行 F96（remote-exec.md §23.3）：重连中的机器不问（它的调用要等重连），快照马上回来，用那台机器上一份的条目
/// 并标“重连中”；连回来之后照常问；机器离开注册表之后它的条目也没了。旧代码在第二份快照上一直等。
#[tokio::test]
async fn a_reconnecting_machine_shows_its_last_entries_without_waiting() {
    use std::sync::atomic::{AtomicBool, Ordering};

    use rust_cli_kernel_api::testing::TestMachines;
    static HANG: AtomicBool = AtomicBool::new(false);
    let machines = TestMachines::new(std::sync::Arc::new(Shells::LOCAL));
    let remote = Shells {
        hang: Some(&HANG),
        ..Shells::REMOTE
    };
    machines.connect("dev-a", std::sync::Arc::new(remote), "/srv/proj");
    let mut setup = Setup::new(std::sync::Arc::new(ScriptedModel::new(Vec::new())));
    setup.set_env(std::sync::Arc::new(Shells::LOCAL));
    setup.machines = Some(machines.clone());
    setup.agents = std::sync::Arc::new(OneAgent::default());
    let built = setup.build().await;
    let rows = |snapshot: &TasksSnapshot| -> Vec<(String, Option<String>, bool)> {
        snapshot
            .background
            .iter()
            .map(|e| (e.id.clone(), e.machine.clone(), e.reconnecting))
            .collect()
    };
    let row = |id: &str, machine: Option<&str>, reconnecting: bool| {
        (id.to_owned(), machine.map(str::to_owned), reconnecting)
    };
    let fresh = built.session.tasks_snapshot().await.unwrap();
    assert_eq!(
        rows(&fresh),
        [
            row("b1", None, false),
            row("r1", Some("dev-a"), false),
            row("agent_1", None, false)
        ]
    );
    // 连接断了：调用一直不回，注册表说它在重连。
    HANG.store(true, Ordering::SeqCst);
    machines.set_reconnecting("dev-a", true);
    let stale = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        built.session.tasks_snapshot(),
    )
    .await
    .expect("the snapshot does not wait for a reconnecting machine")
    .unwrap();
    assert_eq!(
        rows(&stale),
        [
            row("b1", None, false),
            row("r1", Some("dev-a"), true),
            row("agent_1", None, false)
        ]
    );
    // `/tasks` 的列表（带输出末尾）同样不等。
    let listed = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        built.session.background_tasks(),
    )
    .await
    .expect("the list does not wait either")
    .unwrap();
    assert!(listed.iter().any(|e| e.id == "r1" && e.reconnecting));
    // 连回来了：照常问。
    HANG.store(false, Ordering::SeqCst);
    machines.set_reconnecting("dev-a", false);
    let back = built.session.tasks_snapshot().await.unwrap();
    assert_eq!(rows(&back), rows(&fresh));
    // 离开注册表：它的条目没了。
    machines.disconnect("dev-a");
    let gone = built.session.tasks_snapshot().await.unwrap();
    assert_eq!(
        rows(&gone),
        [row("b1", None, false), row("agent_1", None, false)]
    );
}

/// 远端执行 F96（remote-exec.md §23.3）：会话关闭不等重连中的机器（它的 `close_session` 放到后台发）。旧代码在关闭里一直等。
#[tokio::test]
async fn closing_does_not_wait_for_a_reconnecting_machine() {
    use std::sync::atomic::{AtomicBool, Ordering};

    use rust_cli_kernel_api::testing::TestMachines;
    static HANG: AtomicBool = AtomicBool::new(false);
    let machines = TestMachines::new(std::sync::Arc::new(Shells::LOCAL));
    let remote = Shells {
        hang: Some(&HANG),
        ..Shells::REMOTE
    };
    machines.connect("dev-a", std::sync::Arc::new(remote), "/srv/proj");
    let mut setup = Setup::new(std::sync::Arc::new(ScriptedModel::new(Vec::new())));
    setup.set_env(std::sync::Arc::new(Shells::LOCAL));
    setup.machines = Some(machines.clone());
    let built = setup.build().await;
    HANG.store(true, Ordering::SeqCst);
    machines.set_reconnecting("dev-a", true);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        built
            .session
            .close(rust_cli_kernel_api::session_api::EndReason::Other),
    )
    .await
    .expect("closing does not wait for a reconnecting machine");
}
