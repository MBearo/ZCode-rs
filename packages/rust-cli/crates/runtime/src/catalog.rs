//! 会话可用的工具、agent 与 skill（`Kernel::session_catalog`，docs/specs/rust-cli/headless.md §3.3，修复线 F39）：
//! `-p` 的 stream-json 写 `system/init` 时用。工具的取舍与主循环的工具快照同一规则（query `step.rs` 的 `refresh`），
//! 顺序：内置按名字（`localeCompare`），MCP 在后。

use rust_cli_kernel_api::catalog::{CatalogQuery, SessionCatalog};
use rust_cli_kernel_api::mcp_state::WAIT_FOR_MCP_SERVERS;
use rust_cli_kernel_api::tool_search::{TOOL_SEARCH, compare_names, is_deferred};

use crate::usage::UsageAnalyzer;

pub(crate) async fn catalog(usage: &UsageAnalyzer, query: &CatalogQuery) -> SessionCatalog {
    let web_search = usage
        .model
        .capabilities(&query.selection)
        .await
        .is_none_or(|found| found.supports_native_web_search);
    let mut tools: Vec<_> = usage
        .tools
        .snapshot(&query.session)
        .tools
        .into_iter()
        .filter(|tool| tool.is_enabled() && (web_search || !tool.needs_native_web_search()))
        .collect();
    // 同主循环的工具快照：有 ToolSearch 时没有 WaitForMcpServers（`step.rs`）；没有延迟的工具时没有 ToolSearch（query
    // `deferred.rs` 的 `wire_tools`）。只看乐观判断（tools-more.md §3.2.1）：`ENABLE_TOOL_SEARCH` 为关时 ToolSearch 不启用、
    // 不在这里；自动的门槛不看（工具池里照样有 ToolSearch）。修改（工作线 F89）：不再看模型（F84 时禁用表里的模型没有）。
    if tools.iter().any(|tool| tool.name() == TOOL_SEARCH) {
        tools.retain(|tool| tool.name() != WAIT_FOR_MCP_SERVERS);
    }
    let active = tools.iter().any(|tool| tool.name() == TOOL_SEARCH)
        && tools.iter().any(|tool| is_deferred(tool.as_ref()));
    if !active {
        tools.retain(|tool| tool.name() != TOOL_SEARCH);
    }
    tools.sort_by(|left, right| {
        left.is_mcp()
            .cmp(&right.is_mcp())
            .then_with(|| compare_names(left.name(), right.name()))
    });
    let agents = usage
        .agents
        .definitions(&query.workspace)
        .await
        .into_iter()
        .map(|agent| agent.agent_type)
        .collect();
    let skills = usage
        .skills
        .skills(&query.workspace)
        .await
        .iter()
        .filter(|skill| skill.user_invocable)
        .map(|skill| skill.name.clone())
        .collect();
    SessionCatalog {
        tools: tools.iter().map(|tool| tool.name().to_owned()).collect(),
        agents,
        skills,
    }
}
