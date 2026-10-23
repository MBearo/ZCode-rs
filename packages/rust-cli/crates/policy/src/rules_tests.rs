use rust_cli_kernel_api::permission::{PermissionRule, RuleBehavior};

use super::*;
use crate::test_support::rules;

fn rule(tool: &str, content: Option<&str>) -> PermissionRule {
    PermissionRule {
        tool: tool.to_owned(),
        content: content.map(str::to_owned),
    }
}

#[test]
fn rule_strings_round_trip_with_escapes() {
    assert_eq!(parse_rule("Bash"), rule("Bash", None));
    assert_eq!(
        parse_rule("Bash(git status:*)"),
        rule("Bash", Some("git status:*"))
    );
    assert_eq!(parse_rule("Bash()"), rule("Bash", None));
    assert_eq!(parse_rule("Bash(*)"), rule("Bash", None));
    assert_eq!(
        parse_rule(r"Bash(echo \(x\))"),
        rule("Bash", Some("echo (x)"))
    );
    assert_eq!(
        format_rule(&rule("Bash", Some("echo (x)"))),
        r"Bash(echo \(x\))"
    );
    assert_eq!(parse_rule("Task"), rule("Agent", None));
    assert_eq!(parse_rule("Read(//etc/**)"), rule("Read", Some("//etc/**")));
    assert_eq!(
        parse_rule("WebFetch(domain:example.com)"),
        rule("WebFetch", Some("domain:example.com"))
    );
    // 右括号不在末尾：整串是工具名。
    assert_eq!(parse_rule("Bash(x) y"), rule("Bash(x) y", None));
}

#[test]
fn mcp_server_rules_cover_every_tool_of_the_server() {
    assert!(tool_matches("mcp__github", "mcp__github__create_issue"));
    assert!(tool_matches("mcp__github__*", "mcp__github__create_issue"));
    assert!(!tool_matches("mcp__git", "mcp__github__create_issue"));
    assert!(!tool_matches("Bash", "BashOutput"));
}

#[test]
fn lookups_split_whole_tool_and_content_rules() {
    let base = rules(&[
        (RuleBehavior::Deny, "WebFetch"),
        (RuleBehavior::Allow, "Bash(ls:*)"),
    ]);
    let session = [rule("Edit", None)];
    let rules = Rules {
        base: &base,
        session: &session,
    };
    assert!(rules.whole_tool("WebFetch", RuleBehavior::Deny).is_some());
    assert!(rules.whole_tool("Bash", RuleBehavior::Allow).is_none());
    let found = rules.whole_tool("Edit", RuleBehavior::Allow).unwrap();
    assert_eq!(
        found.source,
        rust_cli_kernel_api::permission::RuleSource::Session
    );
    assert_eq!(rules.contents("Bash", RuleBehavior::Allow)[0].0, "ls:*");
}

/// 旧工具名的别名与 hook 的 matcher 是同一张表（kernel-api `tool::LEGACY_TOOL_NAMES`；修复线 F58）。
/// 以前这里只有 4 项别名，不认 `KillBash`、`ListPeers`、`Brief` 与三个 MCP 资源工具的
/// 旧名。`AgentOutputTool` / `BashOutputTool` 已退役，保留，免得用户已有的 deny 规则失效。
#[test]
fn legacy_names_follow_the_shared_table() {
    for (old, new) in [
        ("Task", "Agent"),
        ("KillShell", "TaskStop"),
        ("KillBash", "TaskStop"),
        ("ListPeers", "ListAgents"),
        ("Brief", "SendUserMessage"),
        ("ListMcpResources", "ListMcpResourcesTool"),
        ("ReadMcpResource", "ReadMcpResourceTool"),
        ("ReadMcpResourceDir", "ReadMcpResourceDirTool"),
        ("AgentOutputTool", "TaskOutput"),
        ("BashOutputTool", "TaskOutput"),
    ] {
        assert_eq!(parse_rule(old), rule(new, None), "{old}");
        assert_eq!(
            parse_rule(&format!("{old}(x)")),
            rule(new, Some("x")),
            "{old}"
        );
    }
}
