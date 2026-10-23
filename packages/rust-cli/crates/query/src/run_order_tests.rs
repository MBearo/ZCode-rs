//! 发给模型的工具顺序（docs/specs/rust-cli/tools-more.md §3.3、agents.md §4.2，修复线 F16）：注册表的
//! `keeps_order` 为真时按注册表的顺序（子代理按点名的清单排），否则按名字排序。旧代码总是排序。
use std::sync::Arc;

use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::orchestration::{QueryStop, ToolRegistry};
use rust_cli_kernel_api::testing::{FakeTool, ListTools, ScriptedModel, text_response};
use rust_cli_kernel_api::tool::Tool;

use crate::run::tests::{parts, request, scope};

/// 点名清单的顺序（`Read, Grep`）就是发出的顺序。
struct Declared(Vec<Arc<dyn Tool>>);

impl ToolRegistry for Declared {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.0.clone()
    }

    fn find(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.0.iter().find(|tool| tool.name() == name).cloned()
    }

    fn keeps_order(&self) -> bool {
        true
    }
}

fn tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(FakeTool::new("Read", true, "r")),
        Arc::new(FakeTool::new("Grep", true, "g")),
    ]
}

async fn sent(registry: Arc<dyn ToolRegistry>) -> Vec<String> {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["done"])]));
    let mut parts = parts(model.clone(), Vec::new());
    parts.tools = registry;
    let request = request("hi");
    let (scope, _sink) = scope(parts, &request, Abort::new());
    let stop = crate::run::run(scope, request).await.unwrap();
    assert_eq!(stop, QueryStop::Completed);
    model.requests()[0]
        .tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect()
}

#[tokio::test]
async fn a_registry_that_keeps_its_order_is_sent_in_that_order() {
    assert_eq!(sent(Arc::new(Declared(tools()))).await, ["Read", "Grep"]);
}

#[tokio::test]
async fn other_registries_are_sorted_by_name() {
    assert_eq!(sent(Arc::new(ListTools(tools()))).await, ["Grep", "Read"]);
}
