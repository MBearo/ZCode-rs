//! 整个工具的 deny 规则从工具清单里去掉（docs/specs/rust-cli/tools-files.md §1.1.1，工作线 F69）。
//!
//! 修 bug：以前清单不看 deny 规则，`--disallowedTools Bash` 时模型照样看到 Bash、调用后才被判决链拒绝，被 Bash
//! 藏起来的 Glob / Grep 也不补回，每次搜索都要先被拒一次。现在按判决链当前的规则快照去掉命中整个工具 deny 规则的
//! 工具（工具名相同，或 MCP 服务器规则），没有启用的 Bash 时补回没被 deny 的 Glob / Grep。带 content 的规则、
//! ask / allow 规则不影响清单。

use std::sync::Arc;

use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::mcp_state::McpConnections;
use rust_cli_kernel_api::orchestration::{ToolRegistry, ToolSnapshot};
use rust_cli_kernel_api::permission::{PermissionRules, RuleBehavior};
use rust_cli_kernel_api::tool::Tool;
use rust_cli_policy::PolicyGate;
use rust_cli_policy::rules::Rules;

use crate::registry::BASH;
use crate::{glob, grep};

/// 被 Bash 藏起来、Bash 不在清单里时补回的搜索工具（按这个顺序）。
const SEARCH: [&str; 2] = [glob::NAME, grep::NAME];

/// 按规则快照过滤的注册表：包住基础工具与 MCP 工具，外层（Agent 等）经 [`ToolRegistry::hides`] 过滤自己的工具。
pub struct PolicyFiltered {
    inner: Arc<dyn ToolRegistry>,
    gate: Arc<PolicyGate>,
}

impl PolicyFiltered {
    pub fn new(inner: Arc<dyn ToolRegistry>, gate: Arc<PolicyGate>) -> Self {
        Self { inner, gate }
    }

    /// 规则快照里有整个工具的 deny 规则指向这个名字（会话自己的规则集只用于判决，不影响清单）。
    fn denied(rules: &PermissionRules, name: &str) -> bool {
        Rules {
            base: rules,
            session: &[],
        }
        .whole_tool(name, RuleBehavior::Deny)
        .is_some()
    }

    /// 去掉被 deny 的工具之后还有没有启用的 Bash。
    fn bash_left(&self, rules: &PermissionRules) -> bool {
        !Self::denied(rules, BASH) && self.inner.find(BASH).is_some_and(|bash| bash.is_enabled())
    }

    /// 补回的搜索工具：名字是 Glob / Grep、没有被 deny、Bash 不在清单里时，从里面的注册表取回藏起来的那个。
    fn restored(&self, rules: &PermissionRules, name: &str) -> Option<Arc<dyn Tool>> {
        if !SEARCH.contains(&name) || Self::denied(rules, name) || self.bash_left(rules) {
            return None;
        }
        self.inner.withheld(name)
    }

    fn visible(&self, tools: Vec<Arc<dyn Tool>>) -> Vec<Arc<dyn Tool>> {
        let rules = self.gate.rules();
        let mut kept: Vec<Arc<dyn Tool>> = tools
            .into_iter()
            .filter(|tool| !Self::denied(&rules, tool.name()))
            .collect();
        for name in SEARCH {
            if kept.iter().all(|tool| tool.name() != name) {
                kept.extend(self.restored(&rules, name));
            }
        }
        kept
    }
}

impl ToolRegistry for PolicyFiltered {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.visible(self.inner.tools())
    }

    fn find(&self, name: &str) -> Option<Arc<dyn Tool>> {
        let rules = self.gate.rules();
        match self.inner.find(name) {
            Some(tool) if Self::denied(&rules, tool.name()) => None,
            Some(tool) => Some(tool),
            None => self.restored(&rules, name),
        }
    }

    fn withheld(&self, name: &str) -> Option<Arc<dyn Tool>> {
        let rules = self.gate.rules();
        // 已经补回清单里的不再算藏起来的。
        if Self::denied(&rules, name) || self.restored(&rules, name).is_some() {
            return None;
        }
        self.inner.withheld(name)
    }

    fn hides(&self, name: &str) -> bool {
        Self::denied(&self.gate.rules(), name)
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
