//! 工具定义里的 `machine` 参数（remote-exec.md §20.5.1）：只在有远端机器连上时加，只加给带参数的工具。

use std::sync::Arc;

use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::env_registry::{MachineFacts, MachineSet, RemoteMachine};
use rust_cli_kernel_api::ids::{EnvironmentId, MachineName};
use rust_cli_kernel_api::machine_param::MachineParam;
use rust_cli_kernel_api::model::ToolDefinition;
use rust_cli_kernel_api::orchestration::QueryRequest;
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

/// 工具与它们的定义（同名）：Agent 把机器交给子代理，TaskStop、WebFetch 不带参数，其余在那台机器上执行。
fn tools() -> Vec<Arc<dyn Tool>> {
    [
        ("Agent", MachineParam::Delegates),
        ("Bash", MachineParam::RunsThere),
        ("Edit", MachineParam::RunsThere),
        ("Glob", MachineParam::RunsThere),
        ("Grep", MachineParam::RunsThere),
        ("Monitor", MachineParam::RunsThere),
        ("NotebookEdit", MachineParam::RunsThere),
        ("Read", MachineParam::RunsThere),
        ("TaskStop", MachineParam::Absent),
        ("WebFetch", MachineParam::Absent),
        ("Write", MachineParam::RunsThere),
    ]
    .into_iter()
    .map(|(name, param)| {
        Arc::new(FakeTool::new(name, false, "ok").taking_machine(param)) as Arc<dyn Tool>
    })
    .collect()
}

fn names() -> Vec<ToolDefinition> {
    tools().iter().map(|tool| definition(tool.name())).collect()
}

#[test]
fn without_remote_machines_the_definitions_stay_byte_for_byte() {
    let mut definitions = names();
    let before = serde_json::to_string(&definitions).unwrap();
    add_machine_param(
        &mut definitions,
        &tools(),
        &MachineSet::default(),
        &MachineName::primary(),
    );
    assert_eq!(serde_json::to_string(&definitions).unwrap(), before);
    // 子代理的缺省机器断开、没有别的远端机器时同样不加。
    add_machine_param(
        &mut definitions,
        &tools(),
        &MachineSet::default(),
        &MachineName::new("dev-a"),
    );
    assert_eq!(serde_json::to_string(&definitions).unwrap(), before);
}

#[test]
fn a_connected_machine_adds_the_parameter_to_the_machine_tools() {
    let mut definitions = names();
    add_machine_param(
        &mut definitions,
        &tools(),
        &dev_a(),
        &MachineName::primary(),
    );
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
            "enum": ["primary", "dev-a"],
            "description": "Machine to run on; omit to use the default machine \"primary\"."
        })
    );
    assert_eq!(
        definitions[0].input_schema["properties"]["machine"]["description"],
        "Machine the agent's tools run on by default; omit to use the default machine \"primary\"."
    );
    // 不改 required。
    assert_eq!(definitions[1].input_schema["required"], json!(["x"]));
}

#[test]
fn a_subagent_on_a_remote_machine_names_its_default() {
    let mut definitions = names();
    add_machine_param(
        &mut definitions,
        &tools(),
        &dev_a(),
        &MachineName::new("dev-a"),
    );
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
        Arc::new(FakeTool::new("Bash", false, "ok").taking_machine(MachineParam::RunsThere)),
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
                json!(["primary", "dev-a"])
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
        self.0
            .lock()
            .unwrap()
            .push(request.default_machine.remote().map(str::to_owned));
        Ok(rust_cli_kernel_api::orchestration::BuiltContext {
            system: Vec::new(),
            attachments: Vec::new(),
        })
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
        // 子代理的请求带派生时 Agent 的 `machine`（工作线 F134：以前经输出端的 `default_machine`）。
        let request = QueryRequest {
            default_machine: if remote {
                MachineName::new("dev-a")
            } else {
                MachineName::primary()
            },
            ..request("hi")
        };
        let (scope, _sink) = scope(parts, &request, Abort::new());
        crate::run::run(scope, request).await.unwrap();
        let expected = remote.then(|| "dev-a".to_owned());
        assert_eq!(seen.0.lock().unwrap().first(), Some(&expected), "{remote}");
    }
}
