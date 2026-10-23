//! 三种询问的性质（shell-permissions.md §4.1）：敏感文件 bypass 放行、hook 放行后仍询问；危险删除 bypass 也问、
//! allow 规则盖不过；命令替换太复杂的询问 bypass 放行。

use super::*;

const SENSITIVE: [&str; 3] = ["/w/.bashrc", "/w/.git/config", "/w/.vscode/settings.json"];

async fn write(gate: &PolicyGate, mode: PermissionMode, path: &str) -> PermissionDecision {
    decide(
        gate,
        mode,
        "Write",
        json!({"file_path": path, "content": "x"}),
    )
    .await
}

#[tokio::test]
async fn bypass_and_guarded_write_sensitive_files() {
    let gate = PolicyGate::default();
    for mode in [PermissionMode::BypassPermissions, PermissionMode::Guarded] {
        for path in SENSITIVE {
            assert_eq!(write(&gate, mode, path).await, allow(), "{mode:?} {path}");
        }
    }
}

#[tokio::test]
async fn other_modes_still_ask_for_sensitive_files_even_with_an_allow_rule() {
    let gate = PolicyGate::new(rules(&[(RuleBehavior::Allow, "Write")]));
    for mode in [PermissionMode::Default, PermissionMode::AcceptEdits] {
        assert_eq!(
            ask_message(&write(&gate, mode, "/w/.git/config").await),
            "ZCode requested permissions to edit /w/.git/config which is a sensitive file.",
            "{mode:?}"
        );
    }
}

#[tokio::test]
async fn bypass_keeps_deny_and_ask_rules_on_sensitive_files() {
    let gate = PolicyGate::new(rules(&[
        (RuleBehavior::Deny, "Edit(.git/**)"),
        (RuleBehavior::Ask, "Edit(.bashrc)"),
    ]));
    let mode = PermissionMode::BypassPermissions;
    assert_eq!(
        write(&gate, mode, "/w/.git/config").await,
        deny("Permission to edit /w/.git/config has been denied.")
    );
    // ask 规则排在敏感文件之前：询问的文字是 ask 规则的。
    assert_eq!(
        ask_message(&write(&gate, mode, "/w/.bashrc").await),
        "ZCode requested permissions to write to /w/.bashrc, but you haven't granted it yet."
    );
}

#[tokio::test]
async fn a_hook_cannot_approve_a_sensitive_write_even_in_bypass() {
    let gate = PolicyGate::default();
    let decision = gate
        .check_rules(
            &Named("Write", PermissionDecision::Passthrough),
            &json!({"file_path": "/w/.bashrc", "content": "x"}),
            &context(PermissionMode::BypassPermissions),
        )
        .await;
    assert!(
        matches!(decision, Some(PermissionDecision::Ask { .. })),
        "{decision:?}"
    );
}

// 测试的工作区根 /w 是根下的一级目录，所以按关键目录询问。
const REMOVE_ROOT: &str = "Dangerous rm operation detected: '/w/*'\n\nThis command would remove a critical system directory. This requires explicit approval and cannot be auto-allowed by permission rules.";
const REMOVE_USR: &str = "Dangerous rmdir operation detected: '/usr'\n\nThis command would remove a critical system directory. This requires explicit approval and cannot be auto-allowed by permission rules.";

#[tokio::test]
async fn bypass_and_allow_rules_cannot_approve_a_dangerous_removal() {
    let whole = PolicyGate::new(rules(&[(RuleBehavior::Allow, "Bash")]));
    for (gate, mode) in [
        (&PolicyGate::default(), PermissionMode::BypassPermissions),
        (&whole, PermissionMode::Default),
    ] {
        assert_eq!(
            ask_message(&bash(gate, mode, "rm -rf *").await),
            REMOVE_ROOT,
            "{mode:?}"
        );
    }
    // guarded 的危险命令规则先问（Node 的文字）；它不认的删除按危险删除问。
    let gate = PolicyGate::default();
    assert_eq!(
        ask_message(&bash(&gate, PermissionMode::Guarded, "rm -rf *").await),
        texts::GUARDED_ASK
    );
    assert_eq!(
        ask_message(&bash(&gate, PermissionMode::Guarded, "rmdir /usr").await),
        REMOVE_USR
    );
    let decision = PolicyGate::default()
        .check_rules(
            &Named("Bash", PermissionDecision::Passthrough),
            &json!({"command": "rm -rf *"}),
            &context(PermissionMode::BypassPermissions),
        )
        .await;
    assert!(
        matches!(decision, Some(PermissionDecision::Ask { .. })),
        "{decision:?}"
    );
}

#[tokio::test]
async fn a_too_complex_command_still_runs_in_bypass() {
    let gate = PolicyGate::new(rules(&[(RuleBehavior::Allow, "Bash(echo:*)")]));
    assert_eq!(
        bash(&gate, PermissionMode::BypassPermissions, "echo $(mkdir x)").await,
        allow()
    );
    assert_eq!(
        ask_message(&bash(&gate, PermissionMode::Default, "echo $(mkdir x)").await),
        "A nested command in this command can't be checked before it runs"
    );
}
