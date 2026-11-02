use rust_cli_kernel_api::permission::{PermissionRule, RuleBehavior};

use super::*;
use crate::test_support::rules;

fn rule(tool: &str, content: Option<&str>) -> PermissionRule {
    PermissionRule {
        tool: tool.to_owned(),
        content: content.map(str::to_owned),
    }
}

#[test]
fn lookups_split_whole_tool_and_content_rules() {
    let base = rules(&[
        (RuleBehavior::Deny, "WebFetch"),
        (RuleBehavior::Allow, "Bash(ls:*)"),
    ]);
    let session = [rule("Edit", None)];
    let rules = Rules {
        base: &base,
        session: &session,
    };
    assert!(rules.whole_tool("WebFetch", RuleBehavior::Deny).is_some());
    assert!(rules.whole_tool("Bash", RuleBehavior::Allow).is_none());
    let found = rules.whole_tool("Edit", RuleBehavior::Allow).unwrap();
    assert_eq!(
        found.source,
        rust_cli_kernel_api::permission::RuleSource::Session
    );
    assert_eq!(rules.contents("Bash", RuleBehavior::Allow)[0].0, "ls:*");
}
