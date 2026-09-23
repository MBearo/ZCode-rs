//! Parity with the TS implementation. Fixtures come from
//! `scripts/zcode-cli-rust-bash-fixtures.mjs`, which runs the Node functions.
use super::*;
use serde_json::{Value, json};
use zcode_cli_domain::permission::{Behavior, Rule, RulePolicy};

fn fixtures() -> Value {
    serde_json::from_str(include_str!("../fixtures/bash.json")).unwrap()
}

#[test]
fn every_exported_callback_and_regex_is_implemented() {
    let tables = tables::tables();
    assert!(tables.commands.len() >= 60);
    assert!(!tables.git.is_empty() && !tables.multiword.is_empty());
    assert!(registry::command("npm").is_some());
    assert!(registry::command("constructor").is_none());
}

#[test]
fn read_only_verdicts_match_node() {
    let mut failures = vec![];
    for case in fixtures()["readOnly"].as_array().unwrap() {
        let command = case[0].as_str().unwrap();
        if is_read_only(command, false) != case[1].as_bool().unwrap() {
            failures.push(command.to_owned());
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches: {failures:#?}",
        failures.len()
    );
}

fn behavior(name: &str) -> Behavior {
    match name {
        "allow" => Behavior::Allow,
        "deny" => Behavior::Deny,
        _ => Behavior::Ask,
    }
}

#[test]
fn suggestions_and_rule_matching_match_node() {
    let mut failures = vec![];
    for case in fixtures()["policies"].as_array().unwrap() {
        let command = case["command"].as_str().unwrap();
        if is_read_only(command, false) != case["readOnly"].as_bool().unwrap() {
            failures.push(format!("readOnly {command:?}"));
        }
        let policy = BashRules::new(command, false);
        if case["throws"] == true {
            // Node 在原型链键上抛异常；Rust 退回精确规则（规格中唯一的有意差异）。
            let exact = json!([{"type":"addRules","behavior":"allow",
                "rules":[{"toolName":"Bash","ruleContent":command.trim()}]}]);
            if serde_json::to_value(&policy.suggestions).unwrap() != exact {
                failures.push(format!("fallback {command:?}"));
            }
            continue;
        }
        if serde_json::to_value(&policy.suggestions).unwrap() != case["suggestions"] {
            failures.push(format!(
                "suggestions {command:?}: {} vs {}",
                serde_json::to_value(&policy.suggestions).unwrap(),
                case["suggestions"]
            ));
        }
        for rule in case["rules"].as_array().unwrap() {
            let rules: Vec<Rule> = rule[1]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| Rule {
                    tool_name: "Bash".into(),
                    rule_content: c.as_str().filter(|c| !c.is_empty()).map(str::to_owned),
                })
                .collect();
            let refs: Vec<&Rule> = rules.iter().collect();
            let actual = policy.evaluate(behavior(rule[0].as_str().unwrap()), &refs);
            if actual != rule[2].as_bool().unwrap() {
                failures.push(format!("rules {command:?} {rule}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches: {failures:#?}",
        failures.len()
    );
}

#[test]
fn unsafe_git_context_blocks_only_git() {
    assert!(is_read_only("git status", false));
    assert!(!is_read_only("git status", true));
    assert_eq!(is_read_only("ls", true), is_read_only("ls", false));
}
