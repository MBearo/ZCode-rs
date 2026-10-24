//! Monitor 的命令来源照 Bash 判（工作线 F83，tools-more.md §19.4）：Bash 的规则与只读判定，整个工具的规则看 Monitor；
//! ws 来源的询问来自工具自己的检查，不经这里的内置检查。

use super::*;

async fn monitor(gate: &PolicyGate, mode: PermissionMode, command: &str) -> PermissionDecision {
    decide(
        gate,
        mode,
        "Monitor",
        json!({"description": "d", "command": command}),
    )
    .await
}

#[tokio::test]
async fn monitor_commands_use_the_bash_rules() {
    let gate = PolicyGate::new(rules(&[
        (RuleBehavior::Allow, "Bash(tail:*)"),
        (RuleBehavior::Deny, "Bash(rm:*)"),
    ]));
    assert_eq!(
        monitor(&gate, PermissionMode::DontAsk, "tail -f log").await,
        allow()
    );
    // 只读命令在 dontAsk 下放行，与 Bash 相同。
    assert_eq!(
        monitor(&gate, PermissionMode::DontAsk, "echo hi").await,
        allow()
    );
    assert_eq!(
        monitor(&gate, PermissionMode::DontAsk, "rm -f x").await,
        deny("Permission to use Bash with command rm -f x has been denied.")
    );
    // 不是只读、没有规则：dontAsk 下按 Monitor 拒绝。
    assert!(matches!(
        monitor(&gate, PermissionMode::DontAsk, "touch x").await,
        PermissionDecision::Deny { message } if message.starts_with("Permission to use Monitor has been denied")
    ));
}

#[tokio::test]
async fn a_whole_monitor_rule_allows_any_command() {
    let gate = PolicyGate::new(rules(&[(RuleBehavior::Allow, "Monitor")]));
    assert_eq!(
        monitor(&gate, PermissionMode::DontAsk, "touch x").await,
        allow()
    );
}
