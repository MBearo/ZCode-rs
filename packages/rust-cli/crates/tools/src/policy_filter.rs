//! 整个工具的 deny 规则从工具清单里去掉（docs/specs/rust-cli/tools-files.md §1.1.1，工作线 F69）。
//!
//! 修 bug：以前清单不看 deny 规则，`--disallowedTools Bash` 时模型照样看到 Bash、调用后才被判决链拒绝，被 Bash
//! 藏起来的 Glob / Grep 也不补回，每次搜索都要先被拒一次。现在按判决链当前的规则快照去掉命中整个工具 deny 规则的
//! 工具（工具名相同，或 MCP 服务器规则），没有启用的 Bash 时补回没被 deny 的 Glob / Grep。带 content 的规则、
//! ask / allow 规则不影响清单。

use std::sync::Arc;

use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::mcp_state::McpConnections;
use rust_cli_kernel_api::orchestration::{RuleSnapshots, ToolRegistry, ToolSnapshot};
use rust_cli_kernel_api::permission::{PermissionRules, RuleBehavior};
use rust_cli_kernel_api::tool::Tool;
use rust_cli_kernel_api::tool_search::{SEARCH_TOOLS, search_fallback};

/// 按规则快照过滤的注册表：包住基础工具与 MCP 工具，外层（Agent 等）经 [`ToolRegistry::hides`] 过滤自己的工具。
pub struct PolicyFiltered {
    inner: Arc<dyn ToolRegistry>,
    gate: Arc<dyn RuleSnapshots>,
}

impl PolicyFiltered {
    /// `gate` 是判决链的规则快照（kernel-api `RuleSnapshots`，F125：以前是具体类型 `PolicyGate`）。
    pub fn new(inner: Arc<dyn ToolRegistry>, gate: Arc<dyn RuleSnapshots>) -> Self {
        Self { inner, gate }
    }

    /// 规则快照里有整个工具的 deny 规则指向这个名字（会话自己的规则集只用于判决，不影响清单）。
    fn denied(rules: &PermissionRules, name: &str) -> bool {
        rules.whole_tool(name, RuleBehavior::Deny).is_some()
    }

    /// 补回的搜索工具（kernel-api `tool_search::search_fallback`）：`shown` 里没有启用的 Bash 时，`wanted` 里没被 deny
    /// 的 Glob / Grep 从里面的注册表取回藏起来的那个。
    fn restored<'a>(
        &self,
        rules: &PermissionRules,
        shown: &[Arc<dyn Tool>],
        wanted: impl IntoIterator<Item = &'a str>,
    ) -> Vec<Arc<dyn Tool>> {
        let wanted = wanted.into_iter().filter(|name| !Self::denied(rules, name));
        search_fallback(shown, wanted, |name| self.inner.withheld(name))
    }

    /// 补回的那个搜索工具（`find` / `withheld` 只问一个名字时）：按去掉 deny 之后的 Bash 判断。
    fn restored_one(&self, rules: &PermissionRules, name: &str) -> Option<Arc<dyn Tool>> {
        let bash: Vec<Arc<dyn Tool>> = self
            .inner
            .find(crate::bash::NAME)
            .filter(|bash| !Self::denied(rules, bash.name()))
            .into_iter()
            .collect();
        self.restored(rules, &bash, [name]).pop()
    }

    fn visible(&self, tools: Vec<Arc<dyn Tool>>) -> Vec<Arc<dyn Tool>> {
        let rules = self.gate.current_rules();
        let mut kept: Vec<Arc<dyn Tool>> = tools
            .into_iter()
            .filter(|tool| !Self::denied(&rules, tool.name()))
            .collect();
        let restored = self.restored(&rules, &kept, SEARCH_TOOLS);
        kept.extend(restored);
        kept
    }
}

impl ToolRegistry for PolicyFiltered {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.visible(self.inner.tools())
    }

    fn find(&self, name: &str) -> Option<Arc<dyn Tool>> {
        let rules = self.gate.current_rules();
        match self.inner.find(name) {
            Some(tool) if Self::denied(&rules, tool.name()) => None,
            Some(tool) => Some(tool),
            None => self.restored_one(&rules, name),
        }
    }

    fn withheld(&self, name: &str) -> Option<Arc<dyn Tool>> {
        let rules = self.gate.current_rules();
        // 已经补回清单里的不再算藏起来的。
        if Self::denied(&rules, name) || self.restored_one(&rules, name).is_some() {
            return None;
        }
        self.inner.withheld(name)
    }

    fn hides(&self, name: &str) -> bool {
        Self::denied(&self.gate.current_rules(), name)
    }

    fn keeps_order(&self) -> bool {
        self.inner.keeps_order()
    }

    fn snapshot(&self, session: &SessionId) -> ToolSnapshot {
        let mut snapshot = self.inner.snapshot(session);
        snapshot.tools = self.visible(snapshot.tools);
        snapshot
    }

    fn mcp(&self) -> Option<Arc<dyn McpConnections>> {
        self.inner.mcp()
    }
}

#[cfg(test)]
#[path = "policy_filter_tests.rs"]
mod tests;
