//! “总是允许”的事实（shell-permissions.md §5）。
use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::orchestration::PermissionAnswer;
use rust_cli_kernel_api::permission::PermissionRule;
use serde_json::json;

use super::*;

fn permission() -> InteractionRequest {
    InteractionRequest::Permission {
        machine: Default::default(),
        tool_use: ToolUseId::new("toolu_1"),
        tool: "Bash".to_owned(),
        input: json!({"command": "git status"}),
        message: "ask".to_owned(),
        suggestions: Vec::new(),
        rule_seed: None,
        asked_by: None,
    }
}

#[test]
fn only_allow_with_rules_adds_project_rules() {
    let rule = PermissionRule {
        tool: "Bash".to_owned(),
        content: Some("git status:*".to_owned()),
    };
    let answer = Answer::from(PermissionAnswer::Allow {
        remember: vec![rule.clone()],
    });
    assert_eq!(
        remembered(&permission(), &answer),
        Some(Fact::PermissionRulesAdded {
            behavior: RuleBehavior::Allow,
            rules: vec![rule],
        })
    );
    let plain = Answer::from(PermissionAnswer::Allow {
        remember: Vec::new(),
    });
    assert_eq!(remembered(&permission(), &plain), None);
    let deny = Answer::from(PermissionAnswer::Deny { feedback: None });
    assert_eq!(remembered(&permission(), &deny), None);
    let question = InteractionRequest::Question {
        tool_use: ToolUseId::new("toolu_2"),
        questions: json!([]),
    };
    assert_eq!(remembered(&question, &answer), None);
}
