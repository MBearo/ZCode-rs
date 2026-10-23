//! WaitForMcpServers（docs/specs/rust-cli/ext.md §3.8，修复线 F36）。没有启用 ToolSearch、又有连接中的服务器时
//! 出现在工具表里（谁放进去见 `ExtRegistry` 与 query 的工具快照）；最多等 5 s。描述与 schema 在
//! `prompts/tools/WaitForMcpServers.json`。

use std::collections::HashSet;
use std::sync::Weak;

use async_trait::async_trait;
use rust_cli_kernel_api::manage::McpServerState;
use rust_cli_kernel_api::mcp_state::{
    McpConnections, McpServerView, McpView, WAIT_FOR_MCP_SERVERS, normalize_name,
};
use rust_cli_kernel_api::message::ToolResultContent;
use rust_cli_kernel_api::permission::PermissionDecision;
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};
use serde_json::Value;
use tokio::sync::mpsc;

use super::McpManager;

/// 最多等多久。
const WAIT_MS: u64 = 5_000;

pub(crate) struct WaitForMcpServers {
    manager: Weak<McpManager>,
}

impl WaitForMcpServers {
    pub(crate) fn new(manager: Weak<McpManager>) -> Self {
        Self { manager }
    }
}

/// 要等的服务器：给了 `servers` 用它；否则全部 Pending 的；没有 Pending 时全部失败的。
fn targets(requested: Vec<String>, view: &McpView) -> Vec<String> {
    if !requested.is_empty() {
        return requested;
    }
    let named = |state: McpServerState| -> Vec<String> {
        view.servers
            .iter()
            .filter(|server| server.state == state)
            .map(|server| server.name.clone())
            .collect()
    };
    let pending = named(McpServerState::Pending);
    if pending.is_empty() {
        named(McpServerState::Failed)
    } else {
        pending
    }
}

/// 原名或规范化后的名字在要等的服务器里。
struct Matcher {
    names: Vec<String>,
    normalized: HashSet<String>,
}

impl Matcher {
    fn new(names: Vec<String>) -> Self {
        let normalized = names.iter().map(|name| normalize_name(name)).collect();
        Self { names, normalized }
    }

    fn matches(&self, server: &McpServerView) -> bool {
        self.names.contains(&server.name) || self.normalized.contains(&normalize_name(&server.name))
    }
}

/// 分类后的结果；`cached`、`needsAuth`、`unconfigured` 本工程没有对应状态（ext.md D10、D31）。
#[derive(Debug, Default, PartialEq, Eq)]
struct Outcome {
    connected: Vec<String>,
    failed: Vec<String>,
    pending: Vec<String>,
    disabled: Vec<String>,
    unknown: Vec<String>,
}

impl Outcome {
    fn of(matcher: &Matcher, view: &McpView) -> Self {
        let mut outcome = Self::default();
        let mut seen = HashSet::new();
        for server in view.servers.iter().filter(|server| matcher.matches(server)) {
            seen.insert(normalize_name(&server.name));
            let list = match server.state {
                McpServerState::Connected => &mut outcome.connected,
                McpServerState::Failed => &mut outcome.failed,
                McpServerState::Pending => &mut outcome.pending,
                McpServerState::Disabled => &mut outcome.disabled,
            };
            list.push(server.name.clone());
        }
        outcome.unknown = matcher
            .names
            .iter()
            .filter(|name| !seen.contains(&normalize_name(name)))
            .cloned()
            .collect();
        outcome
    }

    fn ready(&self) -> bool {
        self.failed.is_empty()
            && self.pending.is_empty()
            && self.disabled.is_empty()
            && self.unknown.is_empty()
    }

    /// 结果文本：空的行省略，换行连接。
    fn text(&self) -> String {
        let line = |label: &str, names: &[String]| {
            (!names.is_empty()).then(|| format!("{label}: {}", names.join(", ")))
        };
        let lines = [
            Some(format!("ready: {}", self.ready())),
            line(
                "Connected (their tools are now available \u{2014} call them directly)",
                &self.connected,
            ),
            line("Failed to connect", &self.failed),
            line(
                "Still connecting (try again or proceed without)",
                &self.pending,
            ),
            line("Disabled (ask the user to enable via /mcp)", &self.disabled),
            line(
                "Unknown (no MCP server with this name is configured)",
                &self.unknown,
            ),
        ];
        lines.into_iter().flatten().collect::<Vec<_>>().join("\n")
    }
}

#[async_trait]
impl Tool for WaitForMcpServers {
    fn name(&self) -> &str {
        WAIT_FOR_MCP_SERVERS
    }

    fn input_schema(&self) -> Value {
        rust_cli_prompts::tool(WAIT_FOR_MCP_SERVERS)
            .map(|prompt| prompt.input_schema.clone())
            .unwrap_or(Value::Null)
    }

    fn description(&self, _context: &PromptContext) -> String {
        rust_cli_prompts::tool(WAIT_FOR_MCP_SERVERS)
            .map(|prompt| prompt.description.clone())
            .unwrap_or_default()
    }

    fn is_read_only(&self, _input: &Value) -> bool {
        true
    }

    /// 不能与别的调用并发。
    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        false
    }

    fn max_result_size_chars(&self) -> Option<usize> {
        Some(10_000)
    }

    /// 没有自己的权限检查，默认放行。
    async fn check_permissions(
        &self,
        _input: &Value,
        _context: &ToolUseContext,
    ) -> PermissionDecision {
        PermissionDecision::Allow {
            updated_input: None,
        }
    }

    async fn call(
        &self,
        input: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let manager = self.manager.upgrade().ok_or_else(|| ToolError::Failed {
            message: "the MCP manager has been released".to_owned(),
        })?;
        let requested: Vec<String> = input
            .get("servers")
            .and_then(Value::as_array)
            .map(|servers| {
                servers
                    .iter()
                    .filter_map(|server| server.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let matcher = Matcher::new(targets(requested, &manager.view()));
        let settled = |view: &McpView| {
            !view
                .servers
                .iter()
                .any(|server| matcher.matches(server) && server.state == McpServerState::Pending)
        };
        let view = tokio::select! {
            view = manager.wait(WAIT_MS, &settled) => view,
            () = context.cancel.cancelled() => return Err(ToolError::Cancelled),
        };
        let outcome = Outcome::of(&matcher, &view);
        tracing::debug!(?outcome, "WaitForMcpServers finished");
        Ok(ToolOutput {
            content: vec![ToolResultContent::Text {
                text: outcome.text(),
            }],
            is_error: !outcome.ready(),
            display: Value::Null,
        })
    }
}

#[cfg(test)]
#[path = "wait_tool_tests.rs"]
mod tests;
