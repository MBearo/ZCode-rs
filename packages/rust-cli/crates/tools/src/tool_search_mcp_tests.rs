//! ToolSearch 遇到连接中的 MCP 服务器（tools-more.md §3.5.1，修复线 F36）：点名的服务器、等待与重搜、补充句。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::manage::McpServerState;
use rust_cli_kernel_api::mcp_state::{McpConnections, McpServerView, McpView};
use rust_cli_kernel_api::orchestration::ToolRegistry;
use rust_cli_kernel_api::testing::ScriptedInteractions;
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};

use super::*;
use crate::test_more::{Ctx, output_text};
use crate::tool_search::{ToolCatalog, ToolSearchTool};

#[test]
fn queries_name_servers_by_prefix_and_by_word() {
    let servers = [
        "slack".to_owned(),
        "calendar".to_owned(),
        "my-db".to_owned(),
    ];
    assert_eq!(mentioned_servers("mcp__slow__lookup", &servers), ["slow"]);
    assert_eq!(mentioned_servers("select mcp__a.b", &servers), ["a.b"]);
    assert_eq!(
        mentioned_servers("Send a Slack message to my-db", &servers),
        ["slack", "my-db"]
    );
    // 词边界：`slackbot` 不算点名 slack。
    assert!(mentioned_servers("slackbot", &servers).is_empty());
    assert_eq!(
        mentioned_servers("mcp__slack__post slack", &servers),
        ["slack"]
    );
}

fn view(states: &[(&str, McpServerState)]) -> McpView {
    McpView {
        servers: states
            .iter()
            .map(|(name, state)| McpServerView {
                name: (*name).to_owned(),
                state: *state,
                reconnecting: false,
            })
            .collect(),
        instructions: Vec::new(),
    }
}

/// 视图由测试设置的 MCP 连接。
struct Connections(watch::Sender<McpView>);

#[async_trait]
impl McpConnections for Connections {
    fn view(&self) -> McpView {
        self.0.borrow().clone()
    }

    async fn wait(
        &self,
        wait_ms: u64,
        done: &(dyn for<'v> Fn(&'v McpView) -> bool + Send + Sync),
    ) -> McpView {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(wait_ms);
        let mut changes = self.0.subscribe();
        loop {
            let view = self.view();
            if done(&view) {
                return view;
            }
            tokio::select! {
                _ = changes.changed() => {}
                () = tokio::time::sleep_until(deadline) => return self.view(),
            }
        }
    }

    fn declare_wait_tool(&self, _session: &SessionId) {}

    fn tool_wait_ms(&self) -> u64 {
        10_000
    }
}

#[test]
fn the_note_lists_pending_servers_and_caps_at_thirty() {
    let none = Connections(watch::channel(view(&[("a", McpServerState::Connected)])).0);
    assert_eq!(pending_note(Some(&none)), "");
    assert_eq!(pending_note(None), "");
    let two = Connections(
        watch::channel(view(&[
            ("b", McpServerState::Pending),
            ("a", McpServerState::Pending),
        ]))
        .0,
    );
    assert_eq!(
        pending_note(Some(&two)),
        ". Some MCP servers are still connecting: a, b. Their tools will become available shortly \u{2014} try searching again. If you're looking for a capability rather than a specific tool name, try keywords that might match the server's purpose (e.g., 'slack message', 'calendar event'). Once you find a matching tool, call it directly \u{2014} do not stop after searching."
    );
    let names: Vec<String> = (0..32).map(|index| format!("s{index:02}")).collect();
    let many: Vec<(&str, McpServerState)> = names
        .iter()
        .map(|name| (name.as_str(), McpServerState::Pending))
        .collect();
    let many = Connections(watch::channel(view(&many)).0);
    assert!(
        pending_note(Some(&many))
            .contains("s28, s29, \u{2026}and 2 more. Their tools will become available shortly")
    );
}

/// 一个 MCP 工具（延迟）。
struct Mcp(String);

#[async_trait]
impl Tool for Mcp {
    fn name(&self) -> &str {
        &self.0
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn description(&self, _context: &PromptContext) -> String {
        "Look up a stored value.".to_owned()
    }

    fn is_mcp(&self) -> bool {
        true
    }

    async fn call(
        &self,
        _input: Value,
        _context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        unreachable!("not called")
    }
}

/// 主线程注册表：ToolSearch 加上连上之后才有的 MCP 工具。
struct Registry {
    search: Arc<dyn Tool>,
    late: Mutex<Vec<Arc<dyn Tool>>>,
    mcp: Arc<Connections>,
}

impl ToolRegistry for Registry {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let mut tools = vec![self.search.clone()];
        tools.extend(self.late.lock().unwrap().iter().cloned());
        tools
    }

    fn find(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools().into_iter().find(|tool| tool.name() == name)
    }

    fn mcp(&self) -> Option<Arc<dyn McpConnections>> {
        Some(self.mcp.clone())
    }
}

struct Rig {
    catalog: Arc<ToolCatalog>,
    registry: Arc<Registry>,
}

fn rig(pending: &[&str]) -> Rig {
    let catalog = Arc::new(ToolCatalog::default());
    let states: Vec<(&str, McpServerState)> = pending
        .iter()
        .map(|name| (*name, McpServerState::Pending))
        .collect();
    let registry = Arc::new(Registry {
        search: Arc::new(ToolSearchTool::new(catalog.clone())),
        late: Mutex::default(),
        mcp: Arc::new(Connections(watch::channel(view(&states)).0)),
    });
    let shared: Arc<dyn ToolRegistry> = registry.clone();
    catalog.bind(&shared);
    Rig { catalog, registry }
}

/// 过 `after_ms` 后 `server` 连上，带来 `mcp__<server>__lookup`。
fn connect_after(rig: &Rig, server: &'static str, after_ms: u64) {
    let registry = rig.registry.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(after_ms)).await;
        registry
            .late
            .lock()
            .unwrap()
            .push(Arc::new(Mcp(format!("mcp__{server}__lookup"))));
        let mut view = registry.mcp.view();
        for each in &mut view.servers {
            if each.name == server {
                each.state = McpServerState::Connected;
            }
        }
        registry.mcp.0.send_replace(view);
    });
}

async fn search(rig: &Rig, query: &str) -> (String, u64) {
    let tool = ToolSearchTool::new(rig.catalog.clone());
    let (progress, _) = mpsc::channel(1);
    let context = Ctx::new(Arc::new(ScriptedInteractions::default())).build();
    let start = tokio::time::Instant::now();
    let output = tool
        .call(json!({"query": query}), context, progress)
        .await
        .unwrap();
    let waited = u64::try_from(start.elapsed().as_millis()).unwrap();
    (output_text(&output), waited)
}

#[tokio::test(start_paused = true)]
async fn a_keyword_search_waits_for_connecting_servers_then_finds_their_tools() {
    let rig = rig(&["slow"]);
    connect_after(&rig, "slow", 1_200);
    assert_eq!(
        search(&rig, "lookup").await,
        ("<ref:mcp__slow__lookup>".to_owned(), 1_200)
    );
}

#[tokio::test(start_paused = true)]
async fn a_select_waits_only_for_the_missing_names() {
    let rig = rig(&["slow"]);
    connect_after(&rig, "slow", 800);
    assert_eq!(
        search(&rig, "select:mcp__slow__lookup").await,
        ("<ref:mcp__slow__lookup>".to_owned(), 800)
    );
}

#[tokio::test(start_paused = true)]
async fn it_waits_at_most_five_seconds_and_names_the_pending_servers() {
    let rig = rig(&["slow"]);
    let (text, waited) = search(&rig, "lookup").await;
    assert_eq!(waited, 5_000);
    assert!(text.starts_with(
        "No matching deferred tools found. Some MCP servers are still connecting: slow."
    ));
}

#[tokio::test(start_paused = true)]
async fn a_query_naming_a_server_that_is_not_connecting_does_not_wait() {
    let rig = rig(&["slow"]);
    let (text, waited) = search(&rig, "mcp__other__lookup").await;
    assert_eq!(waited, 0);
    assert!(text.contains("Some MCP servers are still connecting: slow."));
    // 点名的服务器连上了就不再等别的。
    let rig = rig2();
    connect_after(&rig, "fast", 300);
    let (text, waited) = search(&rig, "select:mcp__fast__lookup").await;
    assert_eq!(waited, 300);
    assert_eq!(text, "<ref:mcp__fast__lookup>");
}

fn rig2() -> Rig {
    rig(&["fast", "slow"])
}
