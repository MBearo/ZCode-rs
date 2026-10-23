//! 工具快照的时点（query.md §1.3，修复线 F36）：运行开始与每个工具批之后各取一次，当步 attachment 与之后的请求
//! 共用；启用 ToolSearch 时去掉 WaitForMcpServers；主线程的请求带上它时告诉 MCP 连接。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::manage::McpServerState;
use rust_cli_kernel_api::mcp_state::McpView;
use rust_cli_kernel_api::message::{AttachmentMessage, Message};
use rust_cli_kernel_api::model::{ModelRequest, ModelSelection, QuerySource, SystemBlock};
use rust_cli_kernel_api::orchestration::{
    BuiltContext, ContextBuilder, ContextRequest, OrchestrationError, ToolRegistry,
};
use rust_cli_kernel_api::testing::{FakeTool, ScriptedModel, text_response, tool_use_response};
use rust_cli_kernel_api::tool::Tool;
use serde_json::json;

use crate::mcp_support_tests::{FakeConnections, McpRegistry, server};
use crate::run::tests::{parts, request, scope};

/// 记下每次当步 attachment 收到的工具名与 MCP 状态。
#[derive(Default)]
struct Recording {
    turns: Mutex<Vec<(Vec<String>, Option<McpView>)>>,
}

#[async_trait]
impl ContextBuilder for Recording {
    async fn build(
        &self,
        _request: ContextRequest,
        _history: &[Message],
    ) -> Result<BuiltContext, OrchestrationError> {
        Ok(BuiltContext {
            system: vec![SystemBlock {
                text: "system".to_owned(),
                cache_control: None,
            }],
            user_context: Vec::new(),
            attachments: Vec::new(),
        })
    }

    async fn turn_attachments(
        &self,
        request: ContextRequest,
        _history: &[Message],
    ) -> Result<Vec<AttachmentMessage>, OrchestrationError> {
        self.turns
            .lock()
            .unwrap()
            .push((request.tool_names, request.mcp));
        Ok(Vec::new())
    }
}

/// 每读一次工具表多一个 MCP 工具（读表之间连上的服务器）。
struct Growing {
    base: Arc<dyn Tool>,
    reads: AtomicUsize,
}

impl ToolRegistry for Growing {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let reads = self.reads.fetch_add(1, Ordering::SeqCst);
        let mut tools = vec![self.base.clone()];
        tools.extend((0..reads).map(|index| {
            Arc::new(FakeTool::new(&format!("mcp__late__t{index}"), true, "x")) as Arc<dyn Tool>
        }));
        tools
    }

    fn find(&self, name: &str) -> Option<Arc<dyn Tool>> {
        (name == self.base.name()).then(|| self.base.clone())
    }
}

fn wire_names(request: &ModelRequest) -> Vec<String> {
    request.tools.iter().map(|tool| tool.name.clone()).collect()
}

fn sorted(mut names: Vec<String>) -> Vec<String> {
    names.sort();
    names
}

#[tokio::test]
async fn attachments_and_the_next_request_share_one_tool_snapshot() {
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Echo", &json!({})),
        text_response(&["done"]),
    ]));
    let context = Arc::new(Recording::default());
    let mut parts = parts(model.clone(), Vec::new());
    parts.context = context.clone();
    parts.tools = Arc::new(Growing {
        base: Arc::new(FakeTool::new("Echo", true, "echo")),
        reads: AtomicUsize::new(0),
    });
    let request = request("go");
    let (scope, _) = scope(parts, &request, Abort::new());
    crate::run::run(scope, request).await.unwrap();
    let turns = context.turns.lock().unwrap().clone();
    let requests = model.requests();
    assert_eq!(turns.len(), 2);
    assert_eq!(requests.len(), 2);
    // 缺陷修复（F36）：原来附件读一次表、请求再读一次，请求里多出附件没看到的工具。
    for (turn, request) in turns.iter().zip(&requests) {
        assert_eq!(sorted(turn.0.clone()), sorted(wire_names(request)));
    }
    // 工具批之后刷新：第二步看到新连上的工具。
    assert_eq!(turns[1].0, ["Echo", "mcp__late__t0"]);
}

fn mcp_registry() -> Arc<McpRegistry> {
    let tools: Vec<Arc<dyn Tool>> = ["Echo", "ToolSearch", "WaitForMcpServers"]
        .into_iter()
        .map(|name| Arc::new(FakeTool::new(name, true, name)) as Arc<dyn Tool>)
        .collect();
    McpRegistry::new(
        tools,
        FakeConnections::new(vec![server("slow", McpServerState::Pending)], 10_000),
    )
}

async fn run_with(
    registry: Arc<McpRegistry>,
    model: &str,
    source: QuerySource,
) -> (Vec<ModelRequest>, Vec<(Vec<String>, Option<McpView>)>) {
    let scripted = Arc::new(ScriptedModel::new(vec![text_response(&["done"])]));
    let context = Arc::new(Recording::default());
    let mut parts = parts(scripted.clone(), Vec::new());
    parts.context = context.clone();
    parts.tools = registry;
    let mut request = request("go");
    request.selection = ModelSelection {
        provider: "anthropic".to_owned(),
        model: model.to_owned(),
        reasoning: None,
        connection: None,
    };
    request.source = source;
    let (scope, _) = scope(parts, &request, Abort::new());
    crate::run::run(scope, request).await.unwrap();
    let turns = context.turns.lock().unwrap().clone();
    (scripted.requests(), turns)
}

#[tokio::test]
async fn wait_for_mcp_servers_is_dropped_when_tool_search_is_on() {
    let registry = mcp_registry();
    let (requests, turns) =
        run_with(registry.clone(), "claude-opus-5-5", QuerySource::MainThread).await;
    assert!(!wire_names(&requests[0]).contains(&"WaitForMcpServers".to_owned()));
    assert!(!turns[0].0.contains(&"WaitForMcpServers".to_owned()));
    assert!(registry.mcp.declared.lock().unwrap().is_empty());
    // 当步 attachment 拿到与工具同一时点的 MCP 状态。
    assert_eq!(turns[0].1.as_ref().unwrap().pending(), ["slow"]);
    assert_eq!(registry.snapshots.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn wait_for_mcp_servers_is_sent_and_declared_without_tool_search() {
    let registry = mcp_registry();
    let (requests, _) = run_with(registry.clone(), "glm-4.6", QuerySource::MainThread).await;
    assert!(wire_names(&requests[0]).contains(&"WaitForMcpServers".to_owned()));
    let declared: Vec<String> = registry
        .mcp
        .declared
        .lock()
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(declared, ["sess_1"]);
    // 子代理的请求不算（只在主线程记下）。
    let registry = mcp_registry();
    let (requests, _) = run_with(
        registry.clone(),
        "glm-4.6",
        QuerySource::Subagent("general-purpose".to_owned()),
    )
    .await;
    assert!(wire_names(&requests[0]).contains(&"WaitForMcpServers".to_owned()));
    assert!(registry.mcp.declared.lock().unwrap().is_empty());
}
