//! 子代理的缺省机器（R3，remote-exec.md §20.6）：执行管线按 Agent 的 `machine` 换好机器之后，子代理的运行把它当缺省。

use std::sync::{Arc, PoisonError};

use rust_cli_kernel_api::env_registry::EnvironmentRegistry;
use rust_cli_kernel_api::testing::{TestMachines, UnusedEnvironment};
use serde_json::json;

use crate::test_support::with_machines;

#[tokio::test]
async fn an_agent_on_a_remote_machine_runs_its_tools_there_by_default() {
    let registry = TestMachines::new(Arc::new(UnusedEnvironment));
    registry.connect("dev-a", Arc::new(UnusedEnvironment), "/home/dev/proj");
    for (on, expected) in [
        (registry.machine("dev-a").unwrap(), Some("dev-a")),
        (registry.local_machine(), None),
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
        let seen = harness.seen.lock().unwrap_or_else(PoisonError::into_inner);
        assert_eq!(seen[0].default_machine.as_deref(), expected);
    }
}
