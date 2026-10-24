//! 执行管线的机器路由（remote-exec.md §20.6、§20.8）：按 `machine` 选机器与工作区，不带参数的工具回到本机，hooks
//! 用会话的工作区并带 `machine`，询问带 `machine`。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_kernel_api::env::{EnvPath, OperationContext, Origin, WorkspaceRef};
use rust_cli_kernel_api::env_registry::EnvironmentRegistry as _;
use rust_cli_kernel_api::hooks::{HookEvent, HookOutcome};
use rust_cli_kernel_api::ids::{
    EnvironmentId, Generation, InteractionId, OperationId, OwnerEpoch, RunId, SessionId, ToolUseId,
};
use rust_cli_kernel_api::machine::MachineEnv;
use rust_cli_kernel_api::orchestration::{
    InteractionAnswer, InteractionRequest, Interactions, OrchestrationError,
};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::{
    MemoryStore, TestMachines, UnusedEnvironment, local_services, machine,
};
use rust_cli_kernel_api::tool::PromptContext;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::*;

/// 报告自己在哪台机器、哪个工作区上跑的工具。
struct Where(&'static str);

#[async_trait]
impl Tool for Where {
    fn name(&self) -> &str {
        self.0
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"], "additionalProperties": false})
    }
    fn strips_unknown_keys(&self) -> bool {
        self.0 == "Agent"
    }
    fn description(&self, _context: &PromptContext) -> String {
        String::new()
    }
    async fn call(
        &self,
        input: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput {
            content: vec![ToolResultContent::Text {
                text: format!(
                    "{} {} {}",
                    context.machine.name(),
                    context.workspace.root.as_str(),
                    input
                ),
            }],
            is_error: false,
            display: Value::Null,
        })
    }
}

/// 判决链替身：记下判决时的机器与工作区，按 `ask` 决定放行还是询问。
struct Gate {
    ask: bool,
    seen: Mutex<Vec<(String, String)>>,
}

#[async_trait]
impl PermissionGate for Gate {
    async fn decide(&self, _t: &dyn Tool, _i: &Value, c: &ToolUseContext) -> PermissionDecision {
        self.seen.lock().unwrap().push((
            c.machine.name().to_owned(),
            c.workspace.root.as_str().to_owned(),
        ));
        if self.ask {
            PermissionDecision::Ask {
                message: "May I?".to_owned(),
                suggestions: Vec::new(),
            }
        } else {
            PermissionDecision::Allow {
                updated_input: None,
            }
        }
    }
}

#[derive(Default)]
struct Hooks(Mutex<Vec<HookEvent>>);

#[async_trait]
impl HookRunner for Hooks {
    async fn run(&self, event: HookEvent, _cancel: CancellationToken) -> HookOutcome {
        self.0.lock().unwrap().push(event);
        HookOutcome::default()
    }
}

#[derive(Default)]
struct Asked(Mutex<Vec<Option<String>>>);

#[async_trait]
impl Interactions for Asked {
    async fn ask(
        &self,
        _session: &SessionId,
        request: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        if let InteractionRequest::Permission { machine, .. } = request {
            self.0.lock().unwrap().push(machine);
        }
        Ok(InteractionAnswer {
            interaction: InteractionId::new("i1"),
            answer: json!({"decision": "allow"}),
        })
    }
}

fn context(asked: Arc<Asked>, on: MachineEnv) -> ToolUseContext {
    ToolUseContext {
        session: SessionId::new("sess_1"),
        run: RunId::new("run_1"),
        tool_use: ToolUseId::new("toolu_1"),
        agent: None,
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/work"),
        },
        mode: PermissionMode::Default,
        cancel: CancellationToken::new(),
        machine: on,
        local: local_services(Arc::new(UnusedEnvironment)),
        artifacts: Arc::new(MemoryStore::default()),
        interactions: asked,
        operation: OperationContext {
            environment: EnvironmentId::new("local"),
            generation: Generation::new("g1"),
            owner_epoch: OwnerEpoch::LOCAL,
            operation: OperationId::new("op_1"),
            origin: Origin::Agent {
                session: SessionId::new("sess_1"),
                run: RunId::new("run_1"),
            },
        },
    }
}

struct Ran {
    text: String,
    is_error: bool,
    judged: Vec<(String, String)>,
    events: Vec<HookEvent>,
    asked: Vec<Option<String>>,
}

async fn run(
    registry: Option<Arc<TestMachines>>,
    tool: &'static str,
    input: Value,
    on: Option<MachineEnv>,
    ask: bool,
) -> Ran {
    let gate = Arc::new(Gate {
        ask,
        seen: Mutex::default(),
    });
    let hooks = Arc::new(Hooks::default());
    let asked = Arc::new(Asked::default());
    let mut pipeline = Pipeline::new(gate.clone(), hooks.clone());
    if let Some(registry) = registry {
        pipeline = pipeline.with_machines(registry);
    }
    let on = on.unwrap_or_else(|| machine(Arc::new(UnusedEnvironment)));
    let (progress, _updates) = mpsc::channel(4);
    let output = pipeline
        .execute(
            Arc::new(Where(tool)),
            input,
            context(asked.clone(), on),
            progress,
        )
        .await;
    let text = match &output.content[0] {
        ToolResultContent::Text { text } => text.clone(),
        other => panic!("{other:?}"),
    };
    Ran {
        text,
        is_error: output.is_error,
        judged: gate.seen.lock().unwrap().clone(),
        events: hooks.0.lock().unwrap().drain(..).collect(),
        asked: asked.0.lock().unwrap().clone(),
    }
}

fn dev_a() -> Arc<TestMachines> {
    let registry = TestMachines::new(Arc::new(UnusedEnvironment));
    registry.connect("dev-a", Arc::new(UnusedEnvironment), "/home/dev/proj");
    registry
}

#[tokio::test]
async fn a_call_naming_a_machine_runs_there_with_its_workspace() {
    let ran = run(
        Some(dev_a()),
        "Bash",
        json!({"command": "pwd", "machine": "dev-a"}),
        None,
        false,
    )
    .await;
    assert!(!ran.is_error, "{}", ran.text);
    // 工具与判决链都在 dev-a 上、用它的工作区；工具看不到 `machine`。
    assert_eq!(ran.text, r#"dev-a /home/dev/proj {"command":"pwd"}"#);
    assert_eq!(
        ran.judged,
        [("dev-a".to_owned(), "/home/dev/proj".to_owned())]
    );
    // hooks 在本机：会话的工作区，payload 带 `machine`，`tool_input` 里没有它。
    let pre = &ran.events[0];
    assert_eq!(pre.workspace.root.as_str(), "/work");
    assert_eq!(pre.payload["machine"], "dev-a");
    assert_eq!(pre.payload["tool_input"], json!({"command": "pwd"}));
    let post = &ran.events[1];
    assert_eq!(post.workspace.root.as_str(), "/work");
    assert_eq!(post.payload["machine"], "dev-a");
}

#[tokio::test]
async fn omitting_the_machine_or_naming_local_stays_local() {
    for input in [
        json!({"command": "pwd"}),
        json!({"command": "pwd", "machine": "local"}),
    ] {
        let ran = run(Some(dev_a()), "Bash", input, None, false).await;
        assert_eq!(ran.text, r#"local /work {"command":"pwd"}"#);
        // 本机调用的 hooks 输入不变：没有 `machine`。
        assert!(ran.events[0].payload.get("machine").is_none());
    }
}

#[tokio::test]
async fn an_unknown_machine_fails_the_schema() {
    let ran = run(
        Some(dev_a()),
        "Bash",
        json!({"command": "pwd", "machine": "gpu-1"}),
        None,
        false,
    )
    .await;
    assert!(ran.is_error);
    assert!(
        ran.text
            .starts_with("<tool_use_error>InputValidationError:"),
        "{}",
        ran.text
    );
    assert!(ran.text.contains("machine"), "{}", ran.text);
    assert!(ran.judged.is_empty() && ran.events.is_empty());
}

#[tokio::test]
async fn without_remote_machines_nothing_changes() {
    // 没有注册表与注册表里没有远端，结果相同：本机跑；写了 `machine` 照原来的 schema 报错。
    for registry in [None, Some(TestMachines::new(Arc::new(UnusedEnvironment)))] {
        let ran = run(
            registry.clone(),
            "Bash",
            json!({"command": "pwd"}),
            None,
            false,
        )
        .await;
        assert_eq!(ran.text, r#"local /work {"command":"pwd"}"#);
        let ran = run(
            registry,
            "Bash",
            json!({"command": "pwd", "machine": "local"}),
            None,
            false,
        )
        .await;
        assert!(ran.is_error);
        assert!(ran.text.contains("InputValidationError"), "{}", ran.text);
    }
}

#[tokio::test]
async fn a_disconnected_default_machine_is_reported() {
    // 子代理的缺省机器 dev-a 断开了：不写 `machine` 的调用报“没连上”，不落回本机。
    let registry = TestMachines::new(Arc::new(UnusedEnvironment));
    let ran = run(
        Some(registry),
        "Bash",
        json!({"command": "pwd"}),
        Some(MachineEnv::unavailable("dev-a")),
        false,
    )
    .await;
    assert!(ran.is_error);
    assert_eq!(
        ran.text,
        "<tool_use_error>Machine \"dev-a\" is not connected.</tool_use_error>"
    );
}

#[tokio::test]
async fn tools_without_the_parameter_run_locally() {
    // 子代理在 dev-a 上：WebFetch 一类的工具仍在本机。
    let registry = dev_a();
    let remote = registry.machine("dev-a").unwrap();
    let ran = run(
        Some(registry),
        "WebFetch",
        json!({"command": "x"}),
        Some(remote),
        false,
    )
    .await;
    assert_eq!(ran.text, r#"local /work {"command":"x"}"#);
}

#[tokio::test]
async fn the_agent_tool_takes_the_machine_but_keeps_the_session_workspace() {
    let ran = run(
        Some(dev_a()),
        "Agent",
        json!({"command": "explore", "machine": "dev-a"}),
        None,
        false,
    )
    .await;
    assert_eq!(ran.text, r#"dev-a /work {"command":"explore"}"#);
}

#[tokio::test]
async fn the_permission_question_names_the_machine() {
    let ran = run(
        Some(dev_a()),
        "Bash",
        json!({"command": "rm x", "machine": "dev-a"}),
        None,
        true,
    )
    .await;
    assert_eq!(ran.asked, [Some("dev-a".to_owned())]);
    // PermissionRequest hook 同样在本机、带 `machine`。
    let request = ran
        .events
        .iter()
        .find(|event| event.name == HookEventName::PermissionRequest)
        .unwrap();
    assert_eq!(request.payload["machine"], "dev-a");
    assert_eq!(request.workspace.root.as_str(), "/work");
    let ran = run(
        Some(dev_a()),
        "Bash",
        json!({"command": "rm x"}),
        None,
        true,
    )
    .await;
    assert_eq!(ran.asked, [None]);
}
