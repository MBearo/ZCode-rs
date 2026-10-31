//! 工具定义里的 `machine` 参数（remote-exec.md §20.5.1）：只在有远端机器连上时加，只加给带参数的工具。

use std::sync::Arc;

use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::env_registry::{MachineFacts, MachineSet, RemoteMachine};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::model::ToolDefinition;
use rust_cli_kernel_api::testing::{
    FakeTool, ScriptedModel, TestMachines, UnusedEnvironment, text_response,
};
use rust_cli_kernel_api::tool::Tool;
use serde_json::{Value, json};

use super::add_machine_param;
use crate::run::tests::{parts, request, scope};

fn definition(name: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.to_owned(),
        description: format!("{name} tool"),
        input_schema: json!({"type": "object", "properties": {"x": {"type": "string"}}, "required": ["x"]}),
        cache_control: None,
    }
}

fn dev_a() -> MachineSet {
    MachineSet {
        version: 1,
        machines: vec![RemoteMachine {
            name: "dev-a".to_owned(),
            workspace: WorkspaceRef {
                environment: EnvironmentId::new("dev-a"),
                root: EnvPath::new("/home/dev/proj"),
            },
            facts: MachineFacts {
                os: "linux".to_owned(),
                arch: "x86_64".to_owned(),
                shell: "/bin/bash".to_owned(),
                home: None,
                path_style: PathStyle::Posix,
            },
        }],
    }
}

fn names() -> Vec<ToolDefinition> {
    [
        "Agent",
        "Bash",
        "Edit",
        "Glob",
        "Grep",
        "Monitor",
        "NotebookEdit",
        "Read",
        "TaskStop",
        "WebFetch",
        "Write",
    ]
    .iter()
    .map(|name| definition(name))
    .collect()
}

#[test]
fn without_remote_machines_the_definitions_stay_byte_for_byte() {
    let mut definitions = names();
    let before = serde_json::to_string(&definitions).unwrap();
    add_machine_param(&mut definitions, &MachineSet::default(), None);
    assert_eq!(serde_json::to_string(&definitions).unwrap(), before);
    // 子代理的缺省机器断开、没有别的远端机器时同样不加。
    add_machine_param(&mut definitions, &MachineSet::default(), Some("dev-a"));
    assert_eq!(serde_json::to_string(&definitions).unwrap(), before);
}

#[test]
fn a_connected_machine_adds_the_parameter_to_the_machine_tools() {
    let mut definitions = names();
    add_machine_param(&mut definitions, &dev_a(), None);
    let with: Vec<&str> = definitions
        .iter()
        .filter(|d| d.input_schema["properties"].get("machine").is_some())
        .map(|d| d.name.as_str())
        .collect();
    assert_eq!(
        with,
        [
            "Agent",
            "Bash",
            "Edit",
            "Glob",
            "Grep",
            "Monitor",
            "NotebookEdit",
            "Read",
            "Write"
        ]
    );
    let bash = &definitions[1].input_schema["properties"]["machine"];
    assert_eq!(
        bash,
        &json!({
            "type": "string",
            "enum": ["local", "dev-a"],
            "description": "Machine to run on; omit to use the local machine."
        })
    );
    assert_eq!(
        definitions[0].input_schema["properties"]["machine"]["description"],
        "Machine the agent's tools run on by default; omit to use the local machine."
    );
    // 不改 required。
    assert_eq!(definitions[1].input_schema["required"], json!(["x"]));
}

#[test]
fn a_subagent_on_a_remote_machine_names_its_default() {
    let mut definitions = names();
    add_machine_param(&mut definitions, &dev_a(), Some("dev-a"));
    assert_eq!(
        definitions[7].input_schema["properties"]["machine"]["description"],
        "Machine to run on; omit to use the default machine \"dev-a\"."
    );
    assert_eq!(
        definitions[0].input_schema["properties"]["machine"]["description"],
        "Machine the agent's tools run on by default; omit to use the default machine \"dev-a\"."
    );
}

fn schema_of<'a>(request: &'a rust_cli_kernel_api::model::ModelRequest, name: &str) -> &'a Value {
    &request
        .tools
        .iter()
        .find(|tool| tool.name == name)
        .unwrap()
        .input_schema
}

#[tokio::test]
async fn the_request_follows_the_registry_at_each_step() {
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(FakeTool::new("Bash", false, "ok")),
        Arc::new(FakeTool::new("WebFetch", false, "ok")),
    ];
    let registry = TestMachines::new(Arc::new(UnusedEnvironment));
    for connected in [false, true] {
        if connected {
            registry.connect("dev-a", Arc::new(UnusedEnvironment), "/home/dev/proj");
        }
        let model = Arc::new(ScriptedModel::new(vec![text_response(&["done"])]));
        let mut parts = parts(model.clone(), tools.clone());
        parts.machines = registry.clone();
        let request = request("hi");
        let (scope, _sink) = scope(parts, &request, Abort::new());
        crate::run::run(scope, request).await.unwrap();
        let sent = &model.requests()[0];
        assert_eq!(schema_of(sent, "WebFetch"), &json!({"type": "object"}));
        if connected {
            assert_eq!(
                schema_of(sent, "Bash")["properties"]["machine"]["enum"],
                json!(["local", "dev-a"])
            );
        } else {
            assert_eq!(schema_of(sent, "Bash"), &json!({"type": "object"}));
        }
    }
}

/// 记下每一步上下文请求的缺省机器。
#[derive(Default)]
struct SeenDefaults(std::sync::Mutex<Vec<Option<String>>>);

#[async_trait::async_trait]
impl rust_cli_kernel_api::orchestration::ContextBuilder for SeenDefaults {
    async fn build(
        &self,
        request: rust_cli_kernel_api::orchestration::ContextRequest,
        _history: &[rust_cli_kernel_api::message::Message],
    ) -> Result<
        rust_cli_kernel_api::orchestration::BuiltContext,
        rust_cli_kernel_api::orchestration::OrchestrationError,
    > {
        self.0.lock().unwrap().push(request.default_machine);
        Ok(rust_cli_kernel_api::orchestration::BuiltContext {
            system: Vec::new(),
            user_context: Vec::new(),
            attachments: Vec::new(),
        })
    }
}

/// 缺省机器在 `machine` 上的输出端（子代理派生时 Agent 的 `machine`）；其余交给记录的输出端。
struct OnMachine(
    Arc<rust_cli_kernel_api::testing::RecordingSink>,
    &'static str,
);

#[async_trait::async_trait]
impl rust_cli_kernel_api::orchestration::Interactions for OnMachine {
    async fn ask(
        &self,
        session: &rust_cli_kernel_api::ids::SessionId,
        request: rust_cli_kernel_api::orchestration::InteractionRequest,
    ) -> Result<
        rust_cli_kernel_api::orchestration::InteractionAnswer,
        rust_cli_kernel_api::orchestration::OrchestrationError,
    > {
        self.0.ask(session, request).await
    }
}

#[async_trait::async_trait]
impl rust_cli_kernel_api::orchestration::RunSink for OnMachine {
    async fn commit_message(
        &self,
        message: rust_cli_kernel_api::message::Message,
    ) -> Result<(), rust_cli_kernel_api::store::StoreError> {
        self.0.commit_message(message).await
    }

    async fn emit(&self, event: rust_cli_kernel_api::orchestration::QueryEvent) -> bool {
        self.0.emit(event).await
    }

    async fn default_machine(&self) -> Option<String> {
        Some(self.1.to_owned())
    }
}

/// F98（remote-exec.md §25.2.1）：子代理的缺省机器经上下文请求交给 context（机器清单提醒按它写）；主线程是 None。
/// 旧代码的上下文请求没有这一项，提醒总说“不写就在本机”。
#[tokio::test]
async fn the_context_hears_the_default_machine_of_the_run() {
    let registry = TestMachines::new(Arc::new(UnusedEnvironment));
    registry.connect("dev-a", Arc::new(UnusedEnvironment), "/srv/proj");
    for remote in [false, true] {
        let model = Arc::new(ScriptedModel::new(vec![text_response(&["done"])]));
        let seen = Arc::new(SeenDefaults::default());
        let mut parts = parts(model, Vec::new());
        parts.machines = registry.clone();
        parts.context = seen.clone();
        let request = request("hi");
        let (scope, sink) = scope(parts, &request, Abort::new());
        let scope = if remote {
            let mut scope = Arc::into_inner(scope).unwrap();
            scope.sink = Arc::new(OnMachine(sink, "dev-a"));
            Arc::new(scope)
        } else {
            scope
        };
        crate::run::run(scope, request).await.unwrap();
        let expected = remote.then(|| "dev-a".to_owned());
        assert_eq!(seen.0.lock().unwrap().first(), Some(&expected), "{remote}");
    }
}
