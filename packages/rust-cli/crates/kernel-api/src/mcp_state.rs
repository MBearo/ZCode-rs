//! MCP 连接状态的视图与等待（docs/specs/rust-cli/ext.md §3.3、§3.8，修复线 F36）。
//!
//! 每个服务器一行，状态是 pending / connected / failed /
//! disabled。主循环在运行开始与每个工具批之后，把它与工具在同一时点取出（[`crate::orchestration::ToolSnapshot`]），
//! 延迟工具清单里的连接中服务器与 `mcp_instructions_delta` 都看这一份（query.md §1.3）。ToolSearch、
//! WaitForMcpServers 与直接调用连接中服务器的工具时经 [`McpConnections::wait`] 等：连接状态的变化通知加截止
//! 时刻，不轮询；截止时刻用实现方注入的时钟算。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::ids::SessionId;
use crate::listing::ListingEntry;
use crate::manage::McpServerState;

/// 连接中的服务器名多于这么多个时只列前面的：延迟工具清单的
/// 连接中服务器与 ToolSearch 的补充句都用它（两处的列表格式不同）。F58：以前 context 与 tools 各写一份。
pub const PENDING_LISTED: usize = 30;

/// WaitForMcpServers 工具名。
pub const WAIT_FOR_MCP_SERVERS: &str = "WaitForMcpServers";

/// 等连接中的服务器离开 Pending 的上限（毫秒）：WaitForMcpServers 工具与 ToolSearch 搜不到时的等待共用
/// （工作线 F122：以前 ext 与 tools 各写一份 5000）。`-p` 的启动等待是另一个设置（ext 的 `startup`）。
pub const MCP_WAIT_MS: u64 = 5_000;

/// 一个服务器在某一时点的状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerView {
    pub name: String,
    pub state: McpServerState,
    /// 连上过、断开后正在重连（HTTP / SSE，ext.md §3.3）；只在 Pending 时为真。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reconnecting: bool,
}

/// 全部服务器在某一时点的状态，与已连接服务器的 instructions。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpView {
    /// 按服务器名排序。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub servers: Vec<McpServerView>,
    /// 已连接、有 instructions 的服务器（ext.md §3.6）：`name` 是服务器名，`line` 是渲染好的整段。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instructions: Vec<ListingEntry>,
}

impl McpView {
    /// Pending 的服务器名，按码元序（JS `.sort()` 的顺序）。
    pub fn pending(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .servers
            .iter()
            .filter(|server| server.state == McpServerState::Pending)
            .map(|server| server.name.clone())
            .collect();
        names.sort();
        names
    }
}

/// 服务器名的规范化：`[^a-zA-Z0-9_-]` 换成 `_`。
pub fn normalize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// 一个工具名属于哪些服务器：`mcp__<规范化的服务器名>__` 是工具名的前缀，只留前缀最长的那些。
pub fn servers_for_tool<'a>(view: &'a McpView, tool_name: &str) -> Vec<&'a McpServerView> {
    let mut found: Vec<&McpServerView> = Vec::new();
    let mut longest = 0;
    for server in &view.servers {
        let prefix = format!("mcp__{}__", normalize_name(&server.name));
        if !tool_name.starts_with(&prefix) || prefix.len() < longest {
            continue;
        }
        if prefix.len() > longest {
            longest = prefix.len();
            found.clear();
        }
        found.push(server);
    }
    found
}

/// MCP 连接（ext 实现）。
#[async_trait]
pub trait McpConnections: Send + Sync {
    /// 这一时点的状态。
    fn view(&self) -> McpView;

    /// 等到 `done(视图)` 为真，或从现在起过了 `wait_ms` 毫秒；返回最后看到的视图。只在连接状态变化与截止时
    /// 醒来。取消由调用方负责（丢掉这个 future）。
    async fn wait(
        &self,
        wait_ms: u64,
        done: &(dyn for<'v> Fn(&'v McpView) -> bool + Send + Sync),
    ) -> McpView;

    /// 主线程的请求带上了 WaitForMcpServers：这个会话之后一直带着它，直到会话关闭（ext.md §3.8）。
    fn declare_wait_tool(&self, session: &SessionId);

    /// 直接调用连接中服务器的工具时，一次模型响应里最多等多久（`min(MCP_TIMEOUT, 10000)` ms）。
    fn tool_wait_ms(&self) -> u64;
}

#[cfg(test)]
#[path = "mcp_state_tests.rs"]
mod tests;
