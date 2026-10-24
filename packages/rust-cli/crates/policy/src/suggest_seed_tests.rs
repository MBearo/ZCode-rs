use rust_cli_kernel_api::permission::PermissionRule;

use super::*;

fn bash(content: &str) -> PermissionRule {
    PermissionRule {
        tool: "Bash".to_owned(),
        content: Some(content.to_owned()),
    }
}

fn seed(command: &str) -> Option<String> {
    rule_seed(command, &[crate::suggest::suggestion(command)])
}

/// 期望值是对照实现的询问框上实测的种子（docs/specs/rust-cli/shell-permissions.md §4.10）。
#[test]
fn single_commands_seed_from_the_prefix_and_the_specs() {
    for (command, expected) in [
        ("git -C /repo status", "git *"),
        ("mytool --flag", "mytool *"),
        ("docker compose up -d", "docker compose up *"),
        ("kubectl get pods -n prod", "kubectl get pods *"),
        ("FOO=1 mytool build", "FOO=1 mytool build *"),
        (
            "NODE_ENV=production mytool build",
            "NODE_ENV=production mytool build *",
        ),
        ("timeout 10 mytool run", "timeout 10 mytool run"),
        ("npm run build", "npm run *"),
    ] {
        assert_eq!(seed(command).as_deref(), Some(expected), "{command}");
    }
}

#[test]
fn compound_commands_use_their_only_rule() {
    assert_eq!(
        rule_seed("cd sub && mytool x", &[bash("mytool x *")]).as_deref(),
        Some("mytool x *")
    );
    assert_eq!(
        rule_seed(
            "mytool a && othertool b c",
            &[bash("mytool a *"), bash("othertool b *")]
        ),
        None
    );
    // 复合命令没有 Bash 的建议时按整条命令算（不细化）。
    assert_eq!(
        rule_seed("mytool a && othertool b", &[]).as_deref(),
        Some("mytool a *")
    );
}

#[test]
fn seeds_with_control_characters_are_dropped() {
    assert_eq!(rule_seed("mytool\u{7}x", &[]), None);
    assert_eq!(rule_seed("cd x && y", &[bash("y\tz")]), None);
}

#[test]
fn labels_drop_the_trailing_wildcard() {
    assert_eq!(rule_label("mytool a *"), "mytool a");
    assert_eq!(rule_label("npm:*"), "npm");
    assert_eq!(rule_label("git status"), "git status");
}
