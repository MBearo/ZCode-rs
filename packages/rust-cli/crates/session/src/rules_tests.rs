//! “总是允许”的事实（shell-permissions.md §5）。
use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::permission::PermissionRule;
use serde_json::json;

use super::*;

fn permission() -> InteractionRequest {
    InteractionRequest::Permission {
        machine: None,
        tool_use: ToolUseId::new("toolu_1"),
        tool: "Bash".to_owned(),
        input: json!({"command": "git status"}),
        message: "ask".to_owned(),
        suggestions: Vec::new(),
        asked_by: None,
    }
}

#[test]
fn only_allow_with_rules_adds_project_rules() {
    let rule = PermissionRule {
        tool: "Bash".to_owned(),
        content: Some("git status:*".to_owned()),
    };
    let answer = serde_json::to_value(PermissionAnswer::Allow {
        remember: vec![rule.clone()],
    })
    .unwrap();
    assert_eq!(
        remembered(&permission(), &answer),
        Some(Fact::PermissionRulesAdded {
            behavior: RuleBehavior::Allow,
            rules: vec![rule],
        })
    );
    let plain = serde_json::to_value(PermissionAnswer::Allow {
        remember: Vec::new(),
    })
    .unwrap();
    assert_eq!(remembered(&permission(), &plain), None);
    let deny = serde_json::to_value(PermissionAnswer::Deny { feedback: None }).unwrap();
    assert_eq!(remembered(&permission(), &deny), None);
    let question = InteractionRequest::Question {
        tool_use: ToolUseId::new("toolu_2"),
        questions: json!([]),
    };
    assert_eq!(remembered(&question, &answer), None);
}
