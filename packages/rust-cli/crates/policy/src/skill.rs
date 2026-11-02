//! Skill 工具的内容规则（docs/specs/rust-cli/ext.md §2.4）：`Skill(<名字>)` 精确匹配，`Skill(<前缀>:*)` 匹配以前缀开头的名字。
//!
//! Skill 工具对只有安全字段的 skill 自己返回 `Allow`（[`deny_first`] 仍先查 deny 规则）；其余返回
//! `Passthrough`，由 [`check`] 按规则放行、拒绝或给出询问。

use rust_cli_kernel_api::permission::{PermissionRule, RuleBehavior};

use crate::check::{AskKind, Check, Inputs};
use rust_cli_kernel_api::tool_names;

pub(crate) const TOOL: &str = tool_names::SKILL;

/// 模型给的名字（判决主体里的原文）：去掉首尾空白与前导 `/`。
fn requested(raw: &str) -> String {
    let raw = raw.trim();
    raw.strip_prefix('/').unwrap_or(raw).to_owned()
}

fn matches(rule: &str, name: &str) -> bool {
    match rule.strip_suffix(":*") {
        Some(prefix) => name.starts_with(prefix),
        None => rule == name,
    }
}

fn hit(inputs: &Inputs<'_>, behavior: RuleBehavior, name: &str) -> bool {
    inputs
        .rules
        .contents(TOOL, behavior)
        .iter()
        .any(|(rule, _)| matches(rule, name))
}

fn blocked() -> Check {
    Check::deny("Skill execution blocked by permission rules")
}

/// 工具说这个 skill 安全时：deny 规则仍然先于它。
pub(crate) fn deny_first(raw: &str, inputs: &Inputs<'_>) -> Check {
    if hit(inputs, RuleBehavior::Deny, &requested(raw)) {
        blocked()
    } else {
        Check::Allow
    }
}

/// 工具没有意见时：deny → allow → 询问 `Execute skill: <名字>`，建议 `Skill(<名字>)` 与 `Skill(<名字>:*)`。
pub(crate) fn check(raw: &str, inputs: &Inputs<'_>) -> Check {
    let name = requested(raw);
    if hit(inputs, RuleBehavior::Deny, &name) {
        return blocked();
    }
    if hit(inputs, RuleBehavior::Allow, &name) {
        return Check::Allow;
    }
    let rule = |content: String| PermissionRule {
        tool: TOOL.to_owned(),
        content: Some(content),
    };
    Check::Ask {
        message: format!("Execute skill: {name}"),
        kind: AskKind::Other,
        suggestions: vec![rule(name.clone()), rule(format!("{name}:*"))],
    }
}

#[cfg(test)]
#[path = "skill_tests.rs"]
mod tests;
