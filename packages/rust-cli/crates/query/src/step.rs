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

use rust_cli_kernel_api::mcp_state::McpView;
use rust_cli_kernel_api::model::{ModelSelection, QuerySource};
use rust_cli_kernel_api::orchestration::ContextRequest;
use rust_cli_kernel_api::provider::ModelCapabilities;
use rust_cli_kernel_api::tool::Tool;
use rust_cli_kernel_api::tool_names::WAIT_FOR_MCP_SERVERS;
use rust_cli_kernel_api::tool_search::visible_tools;

use crate::RunScope;

/// 这一步发给模型的工具、同一时点的 MCP 状态与模型的能力。
pub(crate) struct StepTools {
    pub(crate) tools: Vec<Arc<dyn Tool>>,
    pub(crate) mcp: Option<McpView>,
    /// 这一步模型的生效配置（model.md §11）：工具清单、system 提醒的措辞、对话中间的 system 与输出预算都看它。
    /// 每个工具轮取一次，当步 attachment 与之后的请求共用（工作线 F131：以前每个工具轮问三次模型端口）。
    pub(crate) capabilities: Option<ModelCapabilities>,
}

impl StepTools {
    /// 取快照，按 kernel-api 的 `tool_search::visible_tools` 过滤（启用的、模型支持的 WebSearch、有 ToolSearch 时没有
    /// WaitForMcpServers；`system/init` 与 `/context` 用同一个函数，工作线 F131）。
    pub(crate) async fn refresh(scope: &RunScope, selection: &ModelSelection) -> Self {
        let capabilities = scope.parts.model.capabilities(selection).await;
        let snapshot = scope.parts.tools.snapshot(&scope.session);
        Self {
            tools: visible_tools(snapshot.tools, capabilities.as_ref()),
            mcp: snapshot.mcp,
            capabilities,
        }
    }

    /// 组装上下文的请求：会话当前的模式、计划模式标志与工作区（actor 的同一个快照）；工具名、MCP 状态与能力出自
    /// 这一步的快照（query.md §1.3）。
    pub(crate) async fn context_request(
        &self,
        scope: &RunScope,
        selection: &ModelSelection,
        source: &QuerySource,
    ) -> ContextRequest {
        let view = scope.view().await;
        ContextRequest {
            capabilities: self.capabilities,
            agent: scope.agent.clone(),
            tool_names: self.names(),
            plan_flags: view.plan_flags,
            mcp: self.mcp.clone(),
            // 远端执行 F98（remote-exec.md §25.2.1）：机器清单提醒按这次运行的缺省机器写（子代理可能缺省在远端）。
            default_machine: scope.default_machine.clone(),
            ..ContextRequest::new(
                scope.session.clone(),
                view.workspace,
                source.clone(),
                view.mode,
                selection.clone(),
            )
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
