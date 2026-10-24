//! Monitor 工具：定义与期限的两种写法、zod 的报错、ws 的权限、起后台命令并交给会话 actor（tools-more.md §19）。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::env::{
    CommandEvent, EnvError, EnvFacts, EnvPath, FileContent, OperationContext, Origin, Outcome,
    ReadFile, WorkspaceRef, WriteFile, WriteReceipt,
};
use rust_cli_kernel_api::env_shell::{BackgroundStop, ShellState};
use rust_cli_kernel_api::ids::{
    EnvironmentId, Generation, OperationId, OwnerEpoch, RunId, SessionId, ToolUseId,
};
use rust_cli_kernel_api::local_services::{Fetch, Git};
use rust_cli_kernel_api::machine::{
    Background, Checkpoints, Commands, EnvInfo, Files, Pdf, Search, Shell,
};
use rust_cli_kernel_api::message::ToolResultContent;
use rust_cli_kernel_api::orchestration::{
    InteractionAnswer, InteractionRequest, Interactions, OrchestrationError,
};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::{MemoryStore, local_services, machine};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::*;

#[derive(Default)]
struct Env {
    started: Mutex<Vec<StartCommand>>,
    stopped: Mutex<Vec<BackgroundTask>>,
}

fn unused() -> EnvError {
    EnvError::Unsupported {
        what: "unused".to_owned(),
    }
}

#[async_trait]
impl EnvInfo for Env {
    async fn facts(&self, _w: &WorkspaceRef) -> Result<EnvFacts, EnvError> {
        Err(unused())
    }
}

#[async_trait]
impl Files for Env {
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
impl Commands for Env {
    async fn start_command(
        &self,
        _w: &WorkspaceRef,
        _c: OperationContext,
        request: StartCommand,
    ) -> Result<(), EnvError> {
        self.started.lock().unwrap().push(request);
        Ok(())
    }

    fn watch_command(
        &self,
        _operation: OperationId,
        _cursor: u64,
    ) -> BoxStream<'static, Result<CommandEvent, EnvError>> {
        stream::iter([Ok(CommandEvent::Started {
            output: EnvPath::new("/t/tasks/b.output"),
        })])
        .chain(stream::pending())
        .boxed()
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
impl Shell for Env {
    async fn shell_state(&self, _w: &WorkspaceRef, _s: &SessionId) -> Result<ShellState, EnvError> {
        Ok(ShellState {
            cwd: EnvPath::new("/w/sub"),
            outputs: None,
            background: Vec::new(),
        })
    }
}

#[async_trait]
impl Background for Env {
    async fn stop_background_command(
        &self,
        _c: OperationContext,
        request: BackgroundTask,
    ) -> Result<BackgroundStop, EnvError> {
        self.stopped.lock().unwrap().push(request);
        Ok(BackgroundStop::NotFound)
    }
}

impl Search for Env {}
impl Checkpoints for Env {}
impl Pdf for Env {}
impl Git for Env {}
impl Fetch for Env {}

/// 会话 actor 的替身：记下交来的 monitor；`refuse` 时像不持有会话的实现那样拒绝。
#[derive(Default)]
struct Owner {
    armed: Mutex<Vec<MonitorArmed>>,
    refuse: bool,
}

#[async_trait]
impl Interactions for Owner {
    async fn ask(
        &self,
        _s: &SessionId,
        _r: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        Err(OrchestrationError::Unsupported {
            what: "ask".to_owned(),
        })
    }
    async fn arm_monitor(
        &self,
        _session: &SessionId,
        monitor: MonitorArmed,
    ) -> Result<(), OrchestrationError> {
        if self.refuse {
            return Err(OrchestrationError::Unsupported {
                what: "monitors".to_owned(),
            });
        }
        self.armed.lock().unwrap().push(monitor);
        Ok(())
    }
}

fn context(env: Arc<Env>, owner: Arc<Owner>) -> ToolUseContext {
    ToolUseContext {
        session: SessionId::new("s1"),
        run: RunId::new("r1"),
        tool_use: ToolUseId::new("toolu_7"),
        agent: None,
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/w"),
        },
        mode: PermissionMode::Default,
        cancel: CancellationToken::new(),
        machine: machine(env.clone()),
        local: local_services(env),
        artifacts: Arc::new(MemoryStore::default()),
        interactions: owner,
        operation: OperationContext {
            environment: EnvironmentId::new("local"),
            generation: Generation::new("g"),
            owner_epoch: OwnerEpoch::LOCAL,
            operation: OperationId::new("op_1"),
            origin: Origin::Agent {
                session: SessionId::new("s1"),
                run: RunId::new("r1"),
            },
        },
    }
}

fn text_of(output: &ToolOutput) -> &str {
    match output.content.as_slice() {
        [ToolResultContent::Text { text }] => text,
        _ => panic!("not text"),
    }
}

#[test]
fn the_definition_follows_the_recorded_tool_and_the_session_cap() {
    let tool = MonitorTool::new();
    assert!(tool.should_defer());
    assert_eq!(tool.max_result_size_chars(), Some(10_000));
    assert!(tool.is_concurrency_safe(&json!({})));
    assert!(tool.search_hint().unwrap().starts_with("watch, monitor"));
    let context = PromptContext {
        tool_names: Vec::new(),
        mode: PermissionMode::Default,
    };
    let print = tool.description(&context);
    assert!(print.contains("(default 5 minutes, at most 10 minutes)"));
    assert!(print.contains("**ws source**"));
    let schema = tool.input_schema();
    assert_eq!(
        schema["properties"]["timeout_ms"]["description"],
        "Kill the monitor after this deadline. Default 300000ms. Deadlines above 600000ms are capped to 600000ms. You are notified at expiry and can re-arm."
    );
    let session = MonitorTool::new().with_session_cap(true);
    assert!(
        session
            .description(&context)
            .contains("(default 5 minutes, at most 30 minutes)")
    );
    assert!(
        session.input_schema()["properties"]["timeout_ms"]["description"]
            .as_str()
            .unwrap()
            .contains("Deadlines above 1800000ms are capped to 1800000ms.")
    );
}

#[tokio::test]
async fn validation_reports_zod_issues() {
    let tool = MonitorTool::new();
    let context = context(Arc::new(Env::default()), Arc::new(Owner::default()));
    let both = json!({"description": "d", "command": "x", "ws": {"url": "wss://e.example.com"}});
    let Validation::Invalid { message, .. } = tool.validate_input(&both, &context).await else {
        panic!("accepted");
    };
    assert!(message.starts_with("InputValidationError: [\n  {\n    \"code\": \"custom\""));
    let fine = json!({"description": "d", "command": "tail -f log"});
    assert_eq!(tool.validate_input(&fine, &context).await, Validation::Ok);
}

#[tokio::test]
async fn sockets_always_ask_and_private_addresses_are_denied() {
    let tool = MonitorTool::new();
    let context = context(Arc::new(Env::default()), Arc::new(Owner::default()));
    let private = json!({"description": "d", "ws": {"url": "ws://10.0.0.1:9/feed"}});
    assert_eq!(
        tool.check_permissions(&private, &context).await,
        PermissionDecision::Deny {
            message: "Monitor cannot open a WebSocket to 10.0.0.1: the address is in a private, link-local, or cloud-metadata range.".to_owned()
        }
    );
    let public = json!({"description": "d", "ws": {"url": "wss://e.example.com/s", "protocols": ["v1", "v2"]}});
    assert_eq!(
        tool.check_permissions(&public, &context).await,
        PermissionDecision::Ask {
            message:
                "Monitor will open a WebSocket to wss://e.example.com/s (subprotocols: v1, v2)"
                    .to_owned(),
            suggestions: Vec::new(),
        }
    );
    let command = json!({"description": "d", "command": "tail -f log"});
    assert_eq!(
        tool.check_permissions(&command, &context).await,
        PermissionDecision::Passthrough
    );
}

#[tokio::test]
async fn a_command_starts_in_the_background_and_is_handed_to_the_session() {
    let env = Arc::new(Env::default());
    let owner = Arc::new(Owner::default());
    let (progress, _updates) = mpsc::channel(8);
    let input = json!({"description": "errors", "command": "tail -f log", "timeout_ms": 3_600_000});
    let output = MonitorTool::new()
        .call(input, context(env.clone(), owner.clone()), progress)
        .await
        .unwrap();
    let started = env.started.lock().unwrap().clone();
    let [start] = started.as_slice() else {
        panic!("{started:?}");
    };
    assert_eq!(start.command, "tail -f log");
    assert_eq!(start.cwd.as_str(), "/w/sub");
    assert_eq!(start.cwd_scope, CwdScope::Pinned);
    assert_eq!(start.timeout_ms, None);
    assert!(start.socket.is_none());
    let background = start.background.clone().unwrap();
    assert_eq!(background.kind, BackgroundKind::Monitor);
    assert_eq!(background.tool_use, Some(ToolUseId::new("toolu_7")));
    let armed = owner.armed.lock().unwrap().clone();
    assert_eq!(armed.len(), 1);
    assert_eq!(armed[0].timeout_ms, PRINT_CAP_MS);
    assert_eq!(armed[0].task, start.task);
    assert_eq!(armed[0].source, MonitorSource::Command);
    assert_eq!(armed[0].output.as_str(), "/t/tasks/b.output");
    assert_eq!(
        text_of(&output),
        format!(
            "Monitor started (task {}, expires in 10m unless the source ends first; you get one notice at expiry — re-arm if you still need the watch). You will be notified on each event. Keep working — do not poll or sleep. Events may arrive while you are waiting for the user — an event is not their reply.",
            start.task
        )
    );
    assert!(start.task.as_str().starts_with('b'));
}

#[tokio::test]
async fn a_socket_uses_its_own_task_prefix() {
    let env = Arc::new(Env::default());
    let owner = Arc::new(Owner::default());
    let (progress, _updates) = mpsc::channel(8);
    let input =
        json!({"description": "feed", "ws": {"url": "wss://e.example.com/s", "protocols": ["v1"]}});
    MonitorTool::new()
        .call(input, context(env.clone(), owner.clone()), progress)
        .await
        .unwrap();
    let start = env.started.lock().unwrap()[0].clone();
    assert!(start.task.as_str().starts_with('s'));
    assert_eq!(start.command, "wss://e.example.com/s");
    assert_eq!(
        start.socket,
        Some(MonitorSocket {
            url: "wss://e.example.com/s".to_owned(),
            protocols: vec!["v1".to_owned()],
        })
    );
    assert_eq!(owner.armed.lock().unwrap()[0].source, MonitorSource::Socket);
    assert_eq!(owner.armed.lock().unwrap()[0].timeout_ms, 300_000);
}

#[tokio::test]
async fn without_a_session_owner_the_command_is_stopped() {
    let env = Arc::new(Env::default());
    let owner = Arc::new(Owner {
        refuse: true,
        ..Owner::default()
    });
    let (progress, _updates) = mpsc::channel(8);
    let input = json!({"description": "errors", "command": "tail -f log"});
    let error = MonitorTool::new()
        .call(input, context(env.clone(), owner), progress)
        .await
        .unwrap_err();
    assert!(matches!(error, ToolError::Failed { .. }));
    assert_eq!(env.stopped.lock().unwrap().len(), 1);
}
