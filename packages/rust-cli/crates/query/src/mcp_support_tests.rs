//! 测试替身：MCP 连接与带 MCP 状态的注册表（ext.md §3.8、query.md §1.3，修复线 F36）。名字以 `_tests.rs` 结尾：
//! 测试文件可以用 `Instant`（README §4）。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::manage::McpServerState;
use rust_cli_kernel_api::mcp_state::{McpConnections, McpServerView, McpView};
use rust_cli_kernel_api::orchestration::{ToolRegistry, ToolSnapshot};
use rust_cli_kernel_api::tool::Tool;
use tokio::sync::watch;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 跟着 tokio 时间走的时钟（暂停的测试时间里也准）。
pub(crate) struct TokioClock(tokio::time::Instant);

impl TokioClock {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self(tokio::time::Instant::now()))
    }
}

impl Clock for TokioClock {
    fn now_ms(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn time_zone(&self) -> String {
        "UTC".to_owned()
    }
}

pub(crate) fn server(name: &str, state: McpServerState) -> McpServerView {
    McpServerView {
        name: name.to_owned(),
        state,
        reconnecting: false,
    }
}

/// MCP 连接：视图由测试设置；`wait` 在视图变化与截止时醒来。
pub(crate) struct FakeConnections {
    view: watch::Sender<McpView>,
    pub(crate) declared: Mutex<Vec<SessionId>>,
    pub(crate) tool_wait: u64,
}

impl FakeConnections {
    pub(crate) fn new(servers: Vec<McpServerView>, tool_wait: u64) -> Arc<Self> {
        let (view, _) = watch::channel(McpView {
            servers,
            instructions: Vec::new(),
        });
        Arc::new(Self {
            view,
            declared: Mutex::default(),
            tool_wait,
        })
    }

    pub(crate) fn set(&self, view: McpView) {
        self.view.send_replace(view);
    }
}

#[async_trait]
impl McpConnections for FakeConnections {
    fn view(&self) -> McpView {
        self.view.borrow().clone()
    }

    async fn wait(
        &self,
        wait_ms: u64,
        done: &(dyn for<'v> Fn(&'v McpView) -> bool + Send + Sync),
    ) -> McpView {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(wait_ms);
        let mut changes = self.view.subscribe();
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

    fn declare_wait_tool(&self, session: &SessionId) {
        lock(&self.declared).push(session.clone());
    }

    fn tool_wait_ms(&self) -> u64 {
        self.tool_wait
    }
}

/// 注册表：固定的工具加上“连上之后才有”的工具；快照带连接的视图。`snapshots` 记取快照的次数。
pub(crate) struct McpRegistry {
    pub(crate) base: Vec<Arc<dyn Tool>>,
    pub(crate) late: Mutex<Vec<Arc<dyn Tool>>>,
    pub(crate) mcp: Arc<FakeConnections>,
    pub(crate) snapshots: AtomicUsize,
}

impl McpRegistry {
    pub(crate) fn new(base: Vec<Arc<dyn Tool>>, mcp: Arc<FakeConnections>) -> Arc<Self> {
        Arc::new(Self {
            base,
            late: Mutex::default(),
            mcp,
            snapshots: AtomicUsize::new(0),
        })
    }

    /// 服务器连上：工具出现、视图变成 Connected（与 ext 一样同时发布）。
    pub(crate) fn connect(&self, name: &str, tools: Vec<Arc<dyn Tool>>) {
        lock(&self.late).extend(tools);
        let mut view = self.mcp.view();
        for server in &mut view.servers {
            if server.name == name {
                server.state = McpServerState::Connected;
            }
        }
        self.mcp.set(view);
    }
}

impl ToolRegistry for McpRegistry {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let mut tools = self.base.clone();
        tools.extend(lock(&self.late).iter().cloned());
        tools
    }

    fn find(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools().into_iter().find(|tool| tool.name() == name)
    }

    fn snapshot(&self, _session: &SessionId) -> ToolSnapshot {
        self.snapshots.fetch_add(1, Ordering::SeqCst);
        ToolSnapshot {
            tools: self.tools(),
            mcp: Some(self.mcp.view()),
        }
    }

    fn mcp(&self) -> Option<Arc<dyn McpConnections>> {
        Some(self.mcp.clone())
    }
}
