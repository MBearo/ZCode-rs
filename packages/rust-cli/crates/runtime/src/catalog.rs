//! 会话可用的工具、agent 与 skill（`Kernel::session_catalog`，docs/specs/rust-cli/headless.md §3.3，修复线 F39）：
//! `-p` 的 stream-json 写 `system/init` 时用。工具的取舍与顺序同主循环：kernel-api `tool_search` 的 `visible_tools`、
//! `search_applies` 与 `model_order`（工作线 F131：以前这里另写一份，靠注释“同主循环”保持一致）。

use rust_cli_kernel_api::catalog::{CatalogQuery, SessionCatalog};
use rust_cli_kernel_api::tool_names::TOOL_SEARCH;
use rust_cli_kernel_api::tool_search::{model_order, search_applies, visible_tools};

use crate::usage::UsageAnalyzer;

pub(crate) async fn catalog(usage: &UsageAnalyzer, query: &CatalogQuery) -> SessionCatalog {
    let capabilities = usage.model.capabilities(&query.selection).await;
    let snapshot = usage.tools.snapshot(&query.session).tools;
    let mut tools = visible_tools(snapshot, capabilities.as_ref());
    // 没有延迟的工具时没有 ToolSearch（同主循环）。只看乐观判断（tools-more.md §3.2.1）：`ENABLE_TOOL_SEARCH` 为关时
    // ToolSearch 不启用、不在这里；自动的门槛不看（工具池里照样有 ToolSearch）。
    if !search_applies(&tools) {
        tools.retain(|tool| tool.name() != TOOL_SEARCH);
    }
    tools.sort_by(|left, right| model_order(left.as_ref(), right.as_ref()));
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
