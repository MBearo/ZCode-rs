//! Parity with Node `edit-matchers.ts`; fixtures from
//! `scripts/zcode-cli-rust-edit-fixtures.mjs`.
use super::*;
use serde_json::{Value, json};

#[test]
fn matches_like_node() {
    let cases: Value = serde_json::from_str(include_str!("../fixtures/edit-match.json")).unwrap();
    for case in cases.as_array().unwrap() {
        let content = case["content"].as_str().unwrap();
        let search = case["search"].as_str().unwrap();
        let replace_all = case["replaceAll"].as_bool().unwrap();
        let (result, replacement) = match find(content, search, replace_all) {
            Match::Matched {
                actual,
                strategy,
                candidates,
            } => {
                let new_string = case["newString"].as_str().unwrap();
                let replacement = preserve_quote_style(
                    search,
                    &actual,
                    &normalize_replacement(strategy, new_string),
                );
                (
                    json!({"status": "matched", "actualString": actual,
                        "strategy": strategy.as_str(), "candidateCount": candidates}),
                    Some(replacement),
                )
            }
            Match::Ambiguous {
                strategy,
                candidates,
            } => (
                json!({"status": "ambiguous", "strategy": strategy.as_str(), "candidateCount": candidates}),
                None,
            ),
            Match::NotFound => (json!({"status": "not_found"}), None),
        };
        assert_eq!(result, case["result"], "{case}");
        assert_eq!(
            replacement.as_deref(),
            case["replacement"].as_str(),
            "{case}"
        );
    }
}

#[test]
fn js_trim_uses_the_js_whitespace_set() {
    assert_eq!(js_trim("\u{feff} a \u{3000}"), "a");
    assert_eq!(js_trim("\u{85}a"), "\u{85}a");
}
