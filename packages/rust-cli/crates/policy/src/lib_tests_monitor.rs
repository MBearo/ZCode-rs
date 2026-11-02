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

/// guarded 下 Monitor 的命令命中危险命令规则时同 Bash 询问一次（shell-permissions.md §4.6.1，F132 的 P1）。
/// 以前条件是“工具是 Bash”，`rm -rf build` 经 Monitor 启动时直接执行；现在按 shell 主体认。
#[tokio::test]
async fn guarded_asks_once_for_a_dangerous_monitor_command() {
    const DANGER: &str = "Dangerous command requires your approval for this execution";
    let gate = PolicyGate::new(rules(&[(RuleBehavior::Allow, "Bash(rm:*)")]));
    // 只批准这一次：执行管线不跑 PermissionRequest hooks（工作线 F137 以前由单独的 `ask_once` 回答）。
    let asked = PermissionDecision::Ask {
        message: DANGER.to_owned(),
        suggestions: Vec::new(),
        once: true,
    };
    // allow 规则不盖过它。
    assert_eq!(
        monitor(&gate, PermissionMode::Guarded, "rm -rf build").await,
        asked
    );
    // 其余命令同 yolo：不询问。
    assert_eq!(
        monitor(&gate, PermissionMode::Guarded, "tail -f log").await,
        allow()
    );
    let tool = Named("Monitor", PermissionDecision::Passthrough);
    let input = json!({"description": "d", "command": "rm -rf build"});
    // hook 放行后的检查仍然询问。
    assert_eq!(
        gate.check_rules(&tool, &input, &context(PermissionMode::Guarded))
            .await,
        Some(asked)
    );
    // yolo 不识别危险命令。
    assert_eq!(
        monitor(&gate, PermissionMode::BypassPermissions, "rm -rf build").await,
        allow()
    );
    // ws 来源没有 shell 主体，不经这一条：工具自己的询问是普通的询问。
    let ws = json!({"description": "d", "ws": {"url": "wss://example.com/feed"}});
    let own = Named("Monitor", PermissionDecision::ask("Connect?", Vec::new()));
    assert_eq!(
        gate.decide(&own, &ws, &context(PermissionMode::Default))
            .await,
        PermissionDecision::ask("Connect?", Vec::new())
    );
    assert!(!matches!(
        gate.decide(&own, &ws, &context(PermissionMode::Guarded))
            .await,
        PermissionDecision::Ask { once: true, .. }
    ));
}
