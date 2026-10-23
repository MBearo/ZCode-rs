//! 直接调用连接中 MCP 服务器的工具（docs/specs/rust-cli/ext.md §3.8，修复线 F36）。
//!
//! `mcp__<server>__…` 不在工具表、它的服务器在首次连接时，执行器把调用排成不可并发的，执行时等到服务器离开
//! Pending（工具与 Connected 一起发布）或用完预算，再找一次工具。预算 `min(MCP_TIMEOUT, 10000)` ms，一次模型响应
//! 里这类等待合计不超过它；一次等满预算仍找不到时，这个服务器在本次运行里不再等（记为 exhausted）。

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use rust_cli_kernel_api::manage::McpServerState;
use rust_cli_kernel_api::mcp_state::{McpView, servers_for_tool};
use rust_cli_kernel_api::tool::Tool;
use tokio_util::sync::CancellationToken;

use crate::RunScope;

/// 本次运行里等满过预算的服务器；按服务器名记（ext.md D33）。
#[derive(Default)]
pub(crate) struct Exhausted(Mutex<HashSet<String>>);

impl Exhausted {
    fn has(&self, server: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(server)
    }

    fn add(&self, servers: impl IntoIterator<Item = String>) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(servers);
    }
}

/// 还要等的服务器：Pending，且本次运行没有等满过。
fn waiting(view: &McpView, name: &str, exhausted: &Exhausted) -> Vec<String> {
    servers_for_tool(view, name)
        .into_iter()
        .filter(|server| server.state == McpServerState::Pending && !exhausted.has(&server.name))
        .map(|server| server.name.clone())
        .collect()
}

/// 要不要等这个找不到的工具：它的服务器在首次连接（Pending、不在重连），本次运行没有等满过。
pub(crate) fn connecting(scope: &RunScope, name: &str) -> bool {
    let Some(mcp) = scope.parts.tools.mcp() else {
        return false;
    };
    servers_for_tool(&mcp.view(), name)
        .into_iter()
        .any(|server| {
            server.state == McpServerState::Pending
                && !server.reconnecting
                && !scope.exhausted.has(&server.name)
        })
}

/// 等工具出现。`waited` 是这次模型响应里这类等待已用的时间。取消或等不到时为 None。
pub(crate) async fn wait_for_tool(
    scope: &RunScope,
    name: &str,
    waited: &AtomicU64,
    cancel: &CancellationToken,
) -> Option<Arc<dyn Tool>> {
    let registry = &scope.parts.tools;
    let mcp = registry.mcp()?;
    let full = mcp.tool_wait_ms();
    let budget = full.saturating_sub(waited.load(Ordering::SeqCst));
    if budget == 0 {
        return registry.find(name);
    }
    let started = scope.parts.clock.now_ms();
    let done = |view: &McpView| waiting(view, name, &scope.exhausted).is_empty();
    let view = tokio::select! {
        view = mcp.wait(budget, &done) => view,
        () = cancel.cancelled() => return None,
    };
    let elapsed = scope.parts.clock.now_ms().saturating_sub(started);
    waited.fetch_add(elapsed, Ordering::SeqCst);
    let found = registry.find(name);
    // 一次给了整份预算、等满了仍没有：这些服务器本次运行不再等（只在一次给了整份预算时记）。
    if found.is_none() && budget == full && elapsed >= full {
        scope.exhausted.add(waiting(&view, name, &scope.exhausted));
    }
    found
}

#[cfg(test)]
#[path = "mcp_wait_tests.rs"]
mod tests;
