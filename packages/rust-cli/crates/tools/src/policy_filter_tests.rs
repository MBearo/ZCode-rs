use std::sync::Arc;

use rust_cli_kernel_api::permission::{PermissionRules, RuleBehavior, RuleSource, SourcedRule};
use rust_cli_kernel_api::testing::FakeTool;
use rust_cli_policy::rules::parse_rule;

use super::*;
use crate::registry::BuiltinTools;

fn rules(denied: &[&str]) -> PermissionRules {
    PermissionRules {
        rules: denied
            .iter()
            .map(|text| SourcedRule {
                source: RuleSource::CliArg,
                behavior: RuleBehavior::Deny,
                rule: parse_rule(text),
            })
            .collect(),
        ..PermissionRules::default()
    }
}

/// 基础工具：Bash、Read、Edit、Write 与被 Bash 藏起来的 Glob / Grep，另有一个 MCP 工具。
fn registry(denied: &[&str]) -> (PolicyFiltered, Arc<PolicyGate>) {
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(FakeTool::new("Bash", false, "ok")),
        Arc::new(FakeTool::new("Read", true, "ok")),
        Arc::new(FakeTool::new("Edit", false, "ok")),
        Arc::new(FakeTool::new("Write", false, "ok")),
        Arc::new(crate::glob::GlobTool::new()),
        Arc::new(crate::grep::GrepTool::new()),
        Arc::new(FakeTool::new("mcp__srv__lookup", true, "ok")),
    ];
    let gate = Arc::new(PolicyGate::new(rules(denied)));
    let inner: Arc<dyn ToolRegistry> = Arc::new(BuiltinTools::new(tools));
    (PolicyFiltered::new(inner, gate.clone()), gate)
}

fn names(tools: &[Arc<dyn Tool>]) -> Vec<&str> {
    tools.iter().map(|tool| tool.name()).collect()
}

/// 没有 deny 规则：与里面的注册表相同（Glob / Grep 仍被 Bash 藏着）。
#[test]
fn without_deny_rules_nothing_changes() {
    let (tools, _) = registry(&[]);
    assert_eq!(
        names(&tools.tools()),
        ["Bash", "Read", "Edit", "Write", "mcp__srv__lookup"]
    );
    assert!(tools.find("Glob").is_none());
    assert!(tools.withheld("Grep").is_some());
}

/// 整个 Bash 被 deny：清单与 `find` 都没有它，原来被它藏起来的 Glob / Grep 补回；Glob 也被 deny 时只补 Grep。
#[test]
fn denying_bash_removes_it_and_restores_the_search_tools() {
    let (tools, _) = registry(&["Bash"]);
    let listed = tools.tools();
    assert_eq!(
        names(&listed),
        ["Read", "Edit", "Write", "mcp__srv__lookup", "Glob", "Grep"]
    );
    assert!(tools.find("Bash").is_none());
    assert_eq!(
        tools
            .find("Grep")
            .map(|tool| tool.name().to_owned())
            .as_deref(),
        Some("Grep")
    );
    // 已经在清单里，不再算“藏起来的”。
    assert!(tools.withheld("Grep").is_none());
    let (tools, _) = registry(&["Bash", "Glob"]);
    assert_eq!(
        names(&tools.tools()),
        ["Read", "Edit", "Write", "mcp__srv__lookup", "Grep"]
    );
    assert!(tools.find("Glob").is_none());
    assert!(tools.withheld("Glob").is_none());
}

/// Edit、Write 与 MCP 服务器的规则去掉对应的工具；带 content 的规则不影响清单。
#[test]
fn only_whole_tool_denials_remove_tools() {
    let (tools, _) = registry(&["Edit", "Write", "mcp__srv"]);
    assert_eq!(names(&tools.tools()), ["Bash", "Read"]);
    assert!(tools.find("Edit").is_none());
    assert!(tools.find("mcp__srv__lookup").is_none());
    assert!(tools.hides("Write"));
    assert!(!tools.hides("Read"));
    let (tools, _) = registry(&["Bash(rm:*)", "Read(/secret/**)"]);
    assert_eq!(
        names(&tools.tools()),
        ["Bash", "Read", "Edit", "Write", "mcp__srv__lookup"]
    );
}

/// 规则快照换了（`/permissions` 改了规则）：下一次取清单就按新规则。
#[test]
fn replaced_rules_apply_on_the_next_listing() {
    let (tools, gate) = registry(&[]);
    assert!(tools.find("Edit").is_some());
    gate.replace_rules(rules(&["Edit"]));
    assert!(tools.find("Edit").is_none());
    assert!(!names(&tools.tools()).contains(&"Edit"));
    let snapshot = tools.snapshot(&rust_cli_kernel_api::ids::SessionId::new("s"));
    assert!(!names(&snapshot.tools).contains(&"Edit"));
}
