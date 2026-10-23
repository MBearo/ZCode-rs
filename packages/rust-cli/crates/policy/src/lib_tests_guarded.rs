//! guarded 模式与 plan 下的判决（修复线 F20，shell-permissions.md §4.6）：危险命令询问一次、没有建议规则，allow
//! 规则与 hook 放行都不盖过它，deny 规则仍然优先；plan 开着时（判决用的模式是 plan）yolo / guarded 也照 plan 询问。

use super::*;

const DANGER: &str = "Dangerous command requires your approval for this execution";

#[tokio::test]
async fn guarded_asks_once_for_dangerous_commands_and_allows_the_rest() {
    let gate = PolicyGate::default();
    let asked = bash(&gate, PermissionMode::Guarded, "rm -rf build").await;
    assert_eq!(
        asked,
        PermissionDecision::Ask {
            message: DANGER.to_owned(),
            suggestions: Vec::new(),
        }
    );
    // 其余命令同 yolo：不询问。
    assert_eq!(
        bash(&gate, PermissionMode::Guarded, "touch x").await,
        allow()
    );
    assert_eq!(
        bash(&gate, PermissionMode::Guarded, "git push origin main").await,
        allow()
    );
    // yolo 不识别危险命令。
    assert_eq!(
        bash(&gate, PermissionMode::BypassPermissions, "rm -rf build").await,
        allow()
    );
    let input = json!({"command": "git reset --hard"});
    let bash_tool = Named("Bash", PermissionDecision::Passthrough);
    assert!(
        gate.ask_once(&bash_tool, &input, &context(PermissionMode::Guarded))
            .await
    );
    assert!(
        !gate
            .ask_once(
                &bash_tool,
                &input,
                &context(PermissionMode::BypassPermissions)
            )
            .await
    );
}

#[tokio::test]
async fn allow_rules_do_not_skip_the_guarded_question_but_deny_rules_win() {
    let gate = PolicyGate::new(rules(&[
        (RuleBehavior::Allow, "Bash(rm:*)"),
        (RuleBehavior::Deny, "Bash(git reset:*)"),
    ]));
    assert_eq!(
        ask_message(&bash(&gate, PermissionMode::Guarded, "rm -rf build").await),
        DANGER
    );
    assert!(matches!(
        bash(&gate, PermissionMode::Guarded, "git reset --hard").await,
        PermissionDecision::Deny { .. }
    ));
    // hook 放行时仍然生效的检查也按 guarded 跑（Node user-once 不让 hook 替用户批准）。
    let input = json!({"command": "rm -rf build"});
    let tool = Named("Bash", PermissionDecision::Passthrough);
    assert_eq!(
        gate.check_rules(&tool, &input, &context(PermissionMode::Guarded))
            .await,
        Some(PermissionDecision::Ask {
            message: DANGER.to_owned(),
            suggestions: Vec::new(),
        })
    );
    assert_eq!(
        gate.check_rules(&tool, &input, &context(PermissionMode::BypassPermissions))
            .await,
        None
    );
}

/// plan 开着时 actor 交给判决链的是 plan（ExecutionState::effective），权限模式是 yolo / guarded 也一样：
/// 写入照 plan 走分类器（不可用时拒绝），危险命令不走 guarded 的询问（Node `!planEnabled && …`）。
#[tokio::test]
async fn plan_overrides_yolo_and_guarded() {
    use rust_cli_kernel_api::permission::ExecutionState;
    let gate = PolicyGate::default();
    for mode in [PermissionMode::BypassPermissions, PermissionMode::Guarded] {
        let state = ExecutionState::from(mode).with_plan(true);
        let decided = bash(&gate, state.effective(), "touch x").await;
        assert_eq!(decided, deny(texts::CLASSIFIER_UNAVAILABLE), "{mode:?}");
        let dangerous = bash(&gate, state.effective(), "rm -rf build").await;
        assert_eq!(dangerous, deny(texts::CLASSIFIER_UNAVAILABLE), "{mode:?}");
        // 关掉 plan 回到原来的权限模式。
        assert_eq!(
            bash(&gate, state.with_plan(false).effective(), "touch x").await,
            allow()
        );
    }
    // auto_in_plan 关掉时 plan 下询问用户（yolo 也问）。
    let asking = PolicyGate::default().with_options(PolicyOptions {
        auto_in_plan: false,
    });
    let state = ExecutionState::from(PermissionMode::BypassPermissions).with_plan(true);
    assert!(matches!(
        bash(&asking, state.effective(), "touch x").await,
        PermissionDecision::Ask { .. }
    ));
}
