//! 只对这一次判决生效的 allow 规则（skill 的 `allowed-tools`，ext.md §2.5；kernel-api `ScopedGate`）。F125 以前这些语义
//! 只在 ext 的测试里经真实的判决链测到（ext 为此依赖 policy）。

use rust_cli_kernel_api::orchestration::ScopedGate;
use rust_cli_kernel_api::permission::syntax::parse_rule;

use super::*;

async fn with_allowed(
    gate: &PolicyGate,
    mode: PermissionMode,
    command: &str,
    allowed: &[&str],
) -> PermissionDecision {
    let allowed: Vec<PermissionRule> = allowed.iter().map(|text| parse_rule(text)).collect();
    gate.decide_with_allowed(
        &Named("Bash", PermissionDecision::Passthrough),
        &json!({ "command": command }),
        &context(mode),
        &allowed,
    )
    .await
}

#[tokio::test]
async fn allowed_tools_admit_a_command_for_one_decision() {
    let gate = PolicyGate::default();
    assert_eq!(
        with_allowed(
            &gate,
            PermissionMode::DontAsk,
            "npm test",
            &["Bash(npm test:*)"]
        )
        .await,
        allow()
    );
    // 没有这条规则时 dontAsk 拒绝；这一次的规则不留在快照里。
    assert!(matches!(
        with_allowed(&gate, PermissionMode::DontAsk, "npm test", &[]).await,
        PermissionDecision::Deny { .. }
    ));
    assert!(gate.rules().rules.is_empty());
}

#[tokio::test]
async fn deny_and_ask_rules_beat_allowed_tools() {
    let gate = PolicyGate::new(rules(&[
        (RuleBehavior::Deny, "Bash(echo:*)"),
        (RuleBehavior::Ask, "Bash(npm test:*)"),
    ]));
    assert!(matches!(
        with_allowed(&gate, PermissionMode::DontAsk, "echo hi", &["Bash(echo:*)"]).await,
        PermissionDecision::Deny { .. }
    ));
    assert!(matches!(
        with_allowed(
            &gate,
            PermissionMode::Default,
            "npm test",
            &["Bash(npm test:*)"]
        )
        .await,
        PermissionDecision::Ask { .. }
    ));
}

/// Skill 工具自己没有意见（带 allowed-tools 的 skill）时，dontAsk 下拒绝，文字说明是 dontAsk。
#[tokio::test]
async fn a_skill_without_its_own_verdict_is_refused_in_dont_ask_mode() {
    let gate = PolicyGate::default();
    let PermissionDecision::Deny { message } = decide(
        &gate,
        PermissionMode::DontAsk,
        "Skill",
        json!({"skill": "greet", "args": "Ada"}),
    )
    .await
    else {
        panic!("expected a denial");
    };
    assert!(
        message.starts_with(
            "Permission to use Skill has been denied because ZCode is running in don't ask mode."
        ),
        "{message}"
    );
}
