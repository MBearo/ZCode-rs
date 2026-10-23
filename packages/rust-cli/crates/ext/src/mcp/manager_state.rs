//! 连接状态的视图、快照与等待（docs/specs/rust-cli/ext.md §3.3.1、§3.3.2、§3.8，修复线 F36）。
//!
//! 等待只在连接状态变化（`changed` 的 watch）与截止时刻醒来；截止时刻用注入的时钟算，不轮询。

use std::collections::BTreeMap;
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::manage::McpServerState;
use rust_cli_kernel_api::mcp_state::{McpConnections, McpServerView, McpView};
use rust_cli_kernel_api::tool::Tool;

use super::{Inner, McpManager, McpStatus, Server, instructions_of, tools_of};
use crate::mcp::startup::{DEFAULT_PREWAIT_MS, segment};

/// 直接调用连接中服务器的工具时，一次模型响应里等待的上限（10 s）。
const TOOL_WAIT_CAP_MS: u64 = 10_000;

fn state_of(status: &McpStatus) -> McpServerState {
    match status {
        McpStatus::Pending => McpServerState::Pending,
        McpStatus::Connected => McpServerState::Connected,
        McpStatus::Failed { .. } => McpServerState::Failed,
        McpStatus::Disabled => McpServerState::Disabled,
    }
}

fn view_of(servers: &BTreeMap<String, Server>) -> McpView {
    McpView {
        servers: servers
            .values()
            .map(|server| McpServerView {
                name: server.config.name.clone(),
                state: state_of(&server.status),
                reconnecting: server.status == McpStatus::Pending && server.reconnecting,
            })
            .collect(),
        instructions: instructions_of(servers),
    }
}

fn pending(server: &Server) -> bool {
    server.status == McpStatus::Pending
}

impl Inner {
    /// 按服务器表判断（只在锁里调用 `check`，`check` 不能再碰管理器）。
    fn check(&self, check: impl Fn(&BTreeMap<String, Server>) -> bool) -> bool {
        check(&self.servers())
    }

    /// 等到 `done` 为真或到 `deadline_ms`（注入时钟的 Unix 毫秒）；返回是否满足。`done` 自己取锁（不在锁里调用，
    /// 调用方的判断可以再查注册表）。
    async fn wait_until(&self, deadline_ms: u64, done: impl Fn() -> bool) -> bool {
        // 先订阅再检查：检查之后的变化一定会唤醒（同 `wait_ready`）。
        let mut changes = self.changed.subscribe();
        loop {
            if done() {
                return true;
            }
            let now = self.clock.now_ms();
            if now >= deadline_ms {
                return false;
            }
            tokio::select! {
                changed = changes.changed() => {
                    // 发送端随管理器存在；关闭说明管理器已释放，不再等。
                    if changed.is_err() {
                        return false;
                    }
                }
                // 醒来不等于到期：回到循环按时钟判断。
                () = tokio::time::sleep(Duration::from_millis(deadline_ms - now)) => {}
            }
        }
    }
}

impl McpManager {
    /// 同一把锁里取出的 MCP 工具与连接状态（query.md §1.3：工具与 `mcp_instructions_delta`、连接中的服务器同一时点）。
    pub fn snapshot(&self) -> (Vec<Arc<dyn Tool>>, McpView) {
        let servers = self.0.servers();
        (tools_of(&self.0, &servers), view_of(&servers))
    }

    /// 有登记的服务器（含停用的）。
    pub fn has_servers(&self) -> bool {
        !self.0.servers().is_empty()
    }

    /// 有 Pending 的服务器。
    pub fn has_pending(&self) -> bool {
        self.0.servers().values().any(pending)
    }

    /// 这个会话的主线程请求带过 WaitForMcpServers（ext.md §3.8）。
    pub fn declared(&self, session: &SessionId) -> bool {
        self.0
            .declared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(session)
    }

    /// 会话关闭：去掉它的 declared（ext.md D34；`/clear`、`/resume` 时清掉）。
    pub fn release(&self, session: &SessionId) {
        self.0
            .declared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(session);
    }

    /// `-p` 的启动等待（ext.md §3.3.1）：① alwaysLoad 的服务器（阻塞时全部）到 `MCP_CONNECT_TIMEOUT_MS`；② 从 ①
    /// 结束起，全部 Pending 的服务器到 prewait 截止；截止小于 2000 时 allow 规则点名的服务器再等到 2000。没连上的
    /// 在后台继续连。
    ///
    /// 缺陷修复（F36）：原来等全部服务器连上或失败，慢服务器拖住第一个请求最多 `MCP_TIMEOUT`。
    pub async fn wait_for_print(&self) {
        let inner = &self.0;
        let startup = &inner.settings.startup;
        let blocks = |server: &Server| !startup.nonblocking || server.config.options.always_load;
        let start = inner.clock.now_ms();
        let deadline = start.saturating_add(startup.connect_wait_ms);
        if !inner
            .wait_until(deadline, || {
                inner.check(|servers| {
                    !servers
                        .values()
                        .any(|server| pending(server) && blocks(server))
                })
            })
            .await
        {
            tracing::info!(
                waited_ms = startup.connect_wait_ms,
                "MCP servers not ready at start-up; proceeding, background connection continues"
            );
        }
        let kick = inner.clock.now_ms();
        // `lifted`（stream-json 输入，headless.md §6）：只等 alwaysLoad 与 allow 规则点名的服务器。
        let waited = |server: &Server| {
            startup.wait_for_deferrable
                || server.config.options.always_load
                || startup.required.contains(&segment(&server.config.name))
        };
        if startup.prewait_ms > 0
            && !inner
                .wait_until(kick.saturating_add(startup.prewait_ms), || {
                    inner.check(|servers| {
                        !servers
                            .values()
                            .any(|server| pending(server) && waited(server))
                    })
                })
                .await
        {
            tracing::info!(
                waited_ms = startup.prewait_ms,
                "MCP servers not ready before the first command; proceeding, background connection continues"
            );
        }
        if startup.prewait_ms < DEFAULT_PREWAIT_MS && !startup.required.is_empty() {
            let required =
                |server: &Server| startup.required.contains(&segment(&server.config.name));
            inner
                .wait_until(kick.saturating_add(DEFAULT_PREWAIT_MS), || {
                    inner.check(|servers| {
                        !servers
                            .values()
                            .any(|server| pending(server) && required(server))
                    })
                })
                .await;
        }
    }
}

#[async_trait]
impl McpConnections for McpManager {
    fn view(&self) -> McpView {
        view_of(&self.0.servers())
    }

    async fn wait(
        &self,
        wait_ms: u64,
        done: &(dyn for<'v> Fn(&'v McpView) -> bool + Send + Sync),
    ) -> McpView {
        let deadline = self.0.clock.now_ms().saturating_add(wait_ms);
        // 视图在锁外交给 `done`：调用方的判断可以再查注册表。
        self.0.wait_until(deadline, || done(&self.view())).await;
        self.view()
    }

    fn declare_wait_tool(&self, session: &SessionId) {
        self.0
            .declared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(session.clone());
    }

    fn tool_wait_ms(&self) -> u64 {
        self.0.settings.connect_timeout_ms.min(TOOL_WAIT_CAP_MS)
    }
}

#[cfg(test)]
#[path = "manager_state_tests.rs"]
mod tests;
