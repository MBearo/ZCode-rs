//! 子代理的工具集（docs/specs/rust-cli/agents.md §4.2）。
//!
//! 每次取工具时按规则从主注册表过滤，不复制工具列表：主注册表是唯一来源（MCP 工具变化时子代理
//! 同样看到）。

use std::sync::Arc;

use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::mcp_state::McpConnections;
use rust_cli_kernel_api::orchestration::{ToolRegistry, ToolSnapshot};
use rust_cli_kernel_api::tool::Tool;

use crate::definitions::AgentDefinition;

/// 所有子代理都不能用的工具（general-purpose 有 Agent、TaskStop、SendMessage，没有
/// Workflow、ScheduleWakeup）。
const ALL_AGENT_DISALLOWED: &[&str] = &[
    "Workflow",
    "ScheduleWakeup",
    "TaskOutput",
    "ExitPlanMode",
    "EnterPlanMode",
    "AskUserQuestion",
];

/// 后台子代理只能用的工具（含 SendMessage、TaskStop）。
const ASYNC_ALLOWED: &[&str] = &[
    "Read",
    "WebSearch",
    "TodoWrite",
    "Grep",
    "WebFetch",
    "Glob",
    "Bash",
    "PowerShell",
    "Edit",
    "Write",
    "NotebookEdit",
    "Skill",
    "StructuredOutput",
    "ToolSearch",
    "EnterWorktree",
    "ExitWorktree",
    "SendMessage",
    "TaskStop",
];

/// 能派生子代理的最大深度（固定值，没有环境变量能改）。
const MAX_SPAWN_DEPTH: u32 = 3;

/// 一个子代理的过滤规则。
#[derive(Debug, Clone)]
pub(crate) struct ToolRule {
    allow: Option<Vec<String>>,
    deny: Vec<String>,
    background: bool,
    /// 这个子代理的深度（主线程派生的是 1）。
    depth: u32,
}

impl ToolRule {
    pub(crate) fn new(definition: &AgentDefinition, background: bool, depth: u32) -> Self {
        Self {
            allow: definition
                .tools
                .clone()
                .filter(|tools| !tools.iter().any(|tool| tool == "*")),
            deny: definition.disallowed_tools.clone(),
            background,
            depth,
        }
    }

    pub(crate) fn allows(&self, name: &str) -> bool {
        let mcp = name.starts_with("mcp__");
        if !mcp && ALL_AGENT_DISALLOWED.contains(&name) {
            return false;
        }
        // Agent 只看深度，不受异步白名单限制（先判 Agent；后台的 general-purpose 有它）。
        // 以前后台子代理一律没有 Agent，子代理里派生的又总在前台，嵌套的后台子代理无从出现。
        let depth_allows = name != crate::agent_tool::NAME || self.depth < MAX_SPAWN_DEPTH;
        if !depth_allows {
            return false;
        }
        if !mcp
            && self.background
            && name != crate::agent_tool::NAME
            && !ASYNC_ALLOWED.contains(&name)
        {
            return false;
        }
        if self.deny.iter().any(|tool| tool == name) {
            return false;
        }
        self.allow
            .as_ref()
            .is_none_or(|allow| allow.iter().any(|tool| tool == name))
    }
}

/// Bash 的名字（有它时搜索交给它，不补回 Glob / Grep）。
const BASH: &str = "Bash";
/// 有 Bash 时主注册表藏起来的搜索工具（tools-files.md §1.1）。
const SEARCH_TOOLS: &[&str] = &["Glob", "Grep"];

/// 子代理看到的工具：主注册表按 [`ToolRule`] 过滤。
///
/// 定义点名了工具清单时（修复线 F16，agents.md §4.2 第 4 条）：按清单的顺序排，发给模型时
/// 保持这个顺序（`keeps_order`）；清单点名 Glob / Grep、解析出的工具里又没有 Bash 时，从主注册表的 `withheld`
/// 补回它们，排在最后。修 bug：以前只从主注册表过滤，被 Bash 藏起来的 Grep 子代理拿不到，顺序也按名字。
pub(crate) struct FilteredTools {
    pub(crate) inner: Arc<dyn ToolRegistry>,
    pub(crate) rule: ToolRule,
}

impl FilteredTools {
    /// 补回的搜索工具（按清单里出现的顺序）；没有点名清单、或解析出的工具里有 Bash 时为空。
    fn restored(&self, resolved: &[Arc<dyn Tool>]) -> Vec<Arc<dyn Tool>> {
        let Some(allow) = &self.rule.allow else {
            return Vec::new();
        };
        if resolved
            .iter()
            .any(|tool| tool.name() == BASH && tool.is_enabled())
        {
            return Vec::new();
        }
        let mut out: Vec<Arc<dyn Tool>> = Vec::new();
        for name in allow {
            if !SEARCH_TOOLS.contains(&name.as_str())
                || !self.rule.allows(name)
                || out.iter().any(|tool| tool.name() == name)
                || resolved.iter().any(|tool| tool.name() == name)
            {
                continue;
            }
            out.extend(self.inner.withheld(name));
        }
        out
    }
}

impl FilteredTools {
    /// 主注册表的工具（`tools()` 或快照里的）按规则过滤、按清单排序。
    fn resolve(&self, tools: Vec<Arc<dyn Tool>>) -> Vec<Arc<dyn Tool>> {
        let pool: Vec<Arc<dyn Tool>> = tools
            .into_iter()
            .filter(|tool| self.rule.allows(tool.name()))
            .collect();
        let Some(allow) = &self.rule.allow else {
            return pool;
        };
        // 按清单的顺序（`allows` 已保证池里的每个工具都在清单里），重复的名字只算一次。
        let mut resolved: Vec<Arc<dyn Tool>> = Vec::with_capacity(pool.len());
        for name in allow {
            if let Some(tool) = pool.iter().find(|tool| tool.name() == name)
                && !resolved.iter().any(|seen| Arc::ptr_eq(seen, tool))
            {
                resolved.push(tool.clone());
            }
        }
        let restored = self.restored(&resolved);
        resolved.extend(restored);
        resolved
    }
}

impl ToolRegistry for FilteredTools {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.resolve(self.inner.tools())
    }

    /// 主注册表的快照按同一规则过滤（MCP 状态不变，query.md §1.3）。
    fn snapshot(&self, session: &SessionId) -> ToolSnapshot {
        let snapshot = self.inner.snapshot(session);
        ToolSnapshot {
            tools: self.resolve(snapshot.tools),
            mcp: snapshot.mcp,
        }
    }

    fn mcp(&self) -> Option<Arc<dyn McpConnections>> {
        self.inner.mcp()
    }

    fn find(&self, name: &str) -> Option<Arc<dyn Tool>> {
        if let Some(tool) = self
            .inner
            .find(name)
            .filter(|tool| self.rule.allows(tool.name()))
        {
            return Some(tool);
        }
        if !SEARCH_TOOLS.contains(&name) {
            return None;
        }
        self.tools().into_iter().find(|tool| tool.name() == name)
    }

    fn keeps_order(&self) -> bool {
        self.rule.allow.is_some()
    }
}

#[cfg(test)]
#[path = "toolset_tests.rs"]
mod tests;
