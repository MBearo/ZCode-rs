//! 对话框里的项目授权（docs/specs/rust-cli/workspace-trust.md §2.4）。
use super::*;

fn rules(texts: &[&str]) -> Vec<String> {
    texts.iter().map(|text| (*text).to_owned()).collect()
}

#[test]
fn rules_are_sorted_and_written_in_display_form() {
    // 期望的顺序：`mcp__x__y, Bash(ls:*), WebFetch(domain:example.com), and Read`。
    let grants = grants(
        "zcode.json",
        &rules(&[
            "Bash(ls:*)",
            "Read",
            "mcp__x__y",
            "WebFetch(domain:example.com)",
        ]),
    )
    .unwrap();
    assert_eq!(
        grants.rules,
        [
            "mcp__x__y",
            "Bash(ls:*)",
            "WebFetch(domain:example.com)",
            "Read"
        ]
    );
    assert_eq!(grants.count, 4);
    assert_eq!(grants.sources, ["zcode.json"]);
    // 四组：特殊工具整条、特殊工具带内容、其他整条、其他带内容；组内保持原顺序。
    let grants = grants_of(&["Grep(x)", "Read", "Edit(src/**)", "Write", "Glob"]);
    assert_eq!(
        grants.rules,
        ["Write", "Edit(src/**)", "Read", "Glob", "Grep(x)"]
    );
}

fn grants_of(texts: &[&str]) -> Grants {
    grants(".zcode/config.json", &rules(texts)).unwrap()
}

#[test]
fn rules_are_cleaned_truncated_and_deduplicated_but_counted_raw() {
    let long = format!("Bash({})", "x".repeat(80));
    let grants = grants_of(&["Bash", "Bash", "Read\u{7}", &long]);
    assert_eq!(grants.count, 4);
    assert_eq!(grants.rules[0], "Bash");
    assert_eq!(grants.rules[1].chars().count(), 61);
    assert!(grants.rules[1].ends_with('…'), "{}", grants.rules[1]);
    assert_eq!(grants.rules[2], "Read");
    assert_eq!(grants.rules.len(), 3);
}

#[test]
fn an_empty_list_has_no_grants() {
    assert_eq!(grants("zcode.json", &[]), None);
}
