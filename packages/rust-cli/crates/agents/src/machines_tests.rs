//! 子代理的缺省机器（R3，remote-exec.md §20.6）：执行管线按 Agent 的 `machine` 换好机器之后，子代理的运行把它当缺省。

use rust_cli_base::sync::LockUnpoisoned;
use rust_cli_kernel_api::ids::MachineName;
use std::sync::Arc;

use rust_cli_kernel_api::env_registry::EnvironmentRegistry;
use rust_cli_kernel_api::testing::{TestMachines, UnusedEnvironment};
use serde_json::json;

use crate::test_support::with_machines;

#[tokio::test]
async fn an_agent_on_a_remote_machine_runs_its_tools_there_by_default() {
    let registry = TestMachines::new(Arc::new(UnusedEnvironment));
    registry.connect("dev-a", Arc::new(UnusedEnvironment), "/home/dev/proj");
    for (on, expected) in [
        (
            registry.machine(&MachineName::new("dev-a")).unwrap(),
            Some("dev-a"),
        ),
        (registry.primary_machine(), None),
    ] {
        let mut harness = with_machines(registry.clone());
        harness.machine = on;
        harness
            .call(
                "sess_parent",
                "Agent",
                json!({"description": "Find", "prompt": "List.", "subagent_type": "Explore", "run_in_background": false}),
            )
            .await;
        let seen = harness.seen.lock_unpoisoned();
        assert_eq!(seen[0].request.default_machine.remote(), expected);
    }
}

/// Agent 带 `machine` 参数、把机器交给子代理（工作区不换）；TaskStop 与发消息的工具不带（remote-exec.md §20.5.1，
/// 工作线 F137：以前是 kernel-api 按名字查的表）。
#[tokio::test]
async fn the_agent_tool_delegates_the_machine_and_the_others_take_none() {
    use rust_cli_kernel_api::machine_param::MachineParam;
    let harness = crate::test_support::harness(None);
    let registry = harness.agents.core.registry().expect("wired");
    let tools = registry.tools();
    let param = |name: &str| {
        tools
            .iter()
            .find(|tool| tool.name() == name)
            .map(|tool| tool.machine_param())
    };
    assert_eq!(param("Agent"), Some(MachineParam::Delegates));
    for name in ["SendMessage", "TaskStop"] {
        assert!(
            matches!(param(name), None | Some(MachineParam::Absent)),
            "{name}"
        );
    }
}
