//! 规则语法（shell-permissions.md §4.2）。判决链里的用法（候选、按行为比较）在 policy 的 `rules_tests.rs`、
//! `shell_rules_tests.rs`。

use super::*;

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

/// 旧工具名的别名与 hook 的 matcher 是同一张表（`tool::LEGACY_TOOL_NAMES`；修复线 F58）。
/// 以前规则这里只有 4 项别名，不认 `KillBash`、`ListPeers`、`Brief` 与三个 MCP 资源工具的
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

#[test]
fn shell_rule_shapes() {
    assert_eq!(
        ShellRule::parse("git status:*"),
        ShellRule::Prefix("git status")
    );
    // 空前缀不是前缀规则。
    assert_eq!(ShellRule::parse(":*"), ShellRule::Exact(":*"));
    assert_eq!(ShellRule::parse("git *"), ShellRule::Wildcard("git *"));
    assert_eq!(ShellRule::parse(r"echo \*"), ShellRule::Exact(r"echo \*"));
    assert_eq!(ShellRule::parse("npm test"), ShellRule::Exact("npm test"));
    assert!(ends_with_wildcard("git * "));
    assert!(!ends_with_wildcard(r"echo \*"));
}

#[test]
fn prefixes_and_wildcards_squeeze_blanks() {
    assert!(prefix_matches("git status", "git status"));
    assert!(prefix_matches("git status", "git  status\t--short"));
    assert!(!prefix_matches("git status", "git statusx"));
    assert!(prefix_matches("rm", "xargs rm -f x"));
    assert!(wildcard_matches("git *", "git"));
    assert!(wildcard_matches("git *", "git\tlog -1"));
    assert!(wildcard_matches("docker * --rm", "docker run --rm"));
    assert!(!wildcard_matches("docker * --rm", "docker run"));
    assert!(wildcard_matches(r"echo \*", "echo *"));
    assert!(!wildcard_matches(r"echo \*", "echo hi"));
    assert!(wildcard_matches("cat src/**/x.rs", "cat src/x.rs"));
    assert!(wildcard_matches("cat src/**/x.rs", "cat src/a/b/x.rs"));
}

/// 列出建议规则时去掉末尾的通配（F133 从 policy 移来：TUI 不再为它依赖 runtime）。
#[test]
fn shell_rule_labels_drop_the_trailing_wildcard() {
    assert_eq!(shell_rule_label("mytool a *"), "mytool a");
    assert_eq!(shell_rule_label("npm:*"), "npm");
    assert_eq!(shell_rule_label("git status"), "git status");
}
