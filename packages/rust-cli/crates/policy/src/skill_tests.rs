//! Skill 的内容规则（ext.md §2.4）。

use rust_cli_kernel_api::permission::{PermissionDecision, PermissionMode, RuleBehavior};
use serde_json::json;

use super::*;
use crate::chain;
use crate::test_support::run;

fn input(name: &str) -> Value {
    json!({"skill": name})
}

#[test]
fn rules_match_the_name_or_a_prefix() {
    assert!(matches("greet", "greet"));
    assert!(!matches("greet", "greeting"));
    assert!(matches("greet:*", "greet"));
    assert!(matches("greet:*", "greeting"));
    assert!(matches("plugin:*", "plugin:deploy"));
    assert!(!matches("other:*", "greet"));
}

#[test]
fn deny_beats_allow_and_a_leading_slash_is_ignored() {
    let entries = [
        (RuleBehavior::Deny, "Skill(greet)"),
        (RuleBehavior::Allow, "Skill(greet:*)"),
    ];
    let check = run(PermissionMode::Default, &entries, |inputs| {
        check(&input("/greet"), inputs)
    });
    assert_eq!(
        check,
        Check::deny("Skill execution blocked by permission rules")
    );
}

#[test]
fn an_allow_rule_admits_and_nothing_asks_to_execute() {
    let allowed = run(
        PermissionMode::Default,
        &[(RuleBehavior::Allow, "Skill(greet)")],
        |inputs| check(&input("greet"), inputs),
    );
    assert_eq!(allowed, Check::Allow);
    let asked = run(PermissionMode::Default, &[], |inputs| {
        check(&input("greet"), inputs)
    });
    let Check::Ask {
        message,
        suggestions,
        ..
    } = asked
    else {
        panic!("expected an ask, got {asked:?}");
    };
    assert_eq!(message, "Execute skill: greet");
    let rendered: Vec<_> = suggestions.iter().map(crate::rules::format_rule).collect();
    assert_eq!(rendered, ["Skill(greet)", "Skill(greet:*)"]);
}

#[test]
fn a_safe_skill_still_meets_deny_rules_first() {
    let safe = PermissionDecision::Allow {
        updated_input: None,
    };
    let denied = run(
        PermissionMode::Default,
        &[(RuleBehavior::Deny, "Skill(greet)")],
        |inputs| chain::own(TOOL, safe.clone(), &input("greet"), inputs).0,
    );
    assert_eq!(
        denied,
        Check::deny("Skill execution blocked by permission rules")
    );
    let allowed = run(PermissionMode::Default, &[], |inputs| {
        chain::own(TOOL, safe.clone(), &input("greet"), inputs).0
    });
    assert_eq!(allowed, Check::Allow);
}
