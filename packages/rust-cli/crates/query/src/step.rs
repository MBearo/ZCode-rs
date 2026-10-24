//! 这一步的工具与 MCP 状态（docs/specs/rust-cli/query.md §1.3，修复线 F36）。
//!
//! 每个工具批之后先刷新工具、再刷新 MCP 客户端，
//! 之后才算当步的 attachment，下一次请求也用这一份工具。本工程在运行开始与每个工具批之后各取一次
//! [`ToolRegistry::snapshot`]，当步 attachment 与之后每一次请求都用它，直到下一个工具批。
//!
//! 缺陷修复（F36）：原来附件读一次注册表、每次请求再读一次。两次之间连上的 MCP 服务器，工具先进请求，
//! `mcp_instructions_delta` 晚一个工具轮。
//!
//! [`ToolRegistry::snapshot`]: rust_cli_kernel_api::orchestration::ToolRegistry::snapshot

use std::sync::Arc;

use rust_cli_kernel_api::mcp_state::{McpView, WAIT_FOR_MCP_SERVERS};
use rust_cli_kernel_api::model::{ModelSelection, QuerySource};
use rust_cli_kernel_api::tool::Tool;
use rust_cli_kernel_api::tool_search::TOOL_SEARCH;

use crate::RunScope;

/// 这一步发给模型的工具与同一时点的 MCP 状态。
pub(crate) struct StepTools {
    pub(crate) tools: Vec<Arc<dyn Tool>>,
    pub(crate) mcp: Option<McpView>,
}

impl StepTools {
    /// 取快照并过滤：启用的工具；去掉当前模型的生效配置不支持的 WebSearch（要 `supportsNativeWebSearch`，修复线
    /// F23，tools-more.md §5；模型端口答不出能力时不去掉）；工具里有 ToolSearch 时去掉 WaitForMcpServers（tools-more.md
    /// §3.3 第 4 条：乐观判断，`ENABLE_TOOL_SEARCH` 为关时 ToolSearch 不启用、不在快照里，工作线 F89）。
    pub(crate) async fn refresh(scope: &RunScope, selection: &ModelSelection) -> Self {
        let capabilities = scope.parts.model.capabilities(selection).await;
        let web_search = capabilities
            .as_ref()
            .is_none_or(|found| found.supports_native_web_search);
        let snapshot = scope.parts.tools.snapshot(&scope.session);
        let mut tools: Vec<Arc<dyn Tool>> = snapshot
            .tools
            .into_iter()
            .filter(|tool| tool.is_enabled() && (web_search || !tool.needs_native_web_search()))
            .collect();
        // 修改（工作线 F89）：不再看模型（F84 时禁用表里的模型不开）；`auto` 而这一步没到门槛时也去掉（乐观判断，tools-more.md §3.2.1）。
        if tools.iter().any(|tool| tool.name() == TOOL_SEARCH) {
            tools.retain(|tool| tool.name() != WAIT_FOR_MCP_SERVERS);
        }
        Self {
            tools,
            mcp: snapshot.mcp,
        }
    }

    pub(crate) fn names(&self) -> Vec<String> {
        self.tools
            .iter()
            .map(|tool| tool.name().to_owned())
            .collect()
    }

    /// 主线程的请求带上 WaitForMcpServers：这个会话之后一直带着它（组装请求时记下；
    /// ext.md §3.8）。
    pub(crate) fn declare(&self, scope: &RunScope, source: &QuerySource) {
        if *source != QuerySource::MainThread
            || !self
                .tools
                .iter()
                .any(|tool| tool.name() == WAIT_FOR_MCP_SERVERS)
        {
            return;
        }
        if let Some(mcp) = scope.parts.tools.mcp() {
            mcp.declare_wait_tool(&scope.session);
        }
    }
}

#[cfg(test)]
#[path = "step_tests.rs"]
mod tests;
