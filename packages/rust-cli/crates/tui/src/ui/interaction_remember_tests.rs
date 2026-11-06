use super::*;

fn bash(content: &str) -> PermissionRule {
    PermissionRule {
        tool: "Bash".to_owned(),
        content: Some(content.to_owned()),
    }
}

#[test]
fn edited_seeds_become_rules() {
    assert_eq!(rules("git *"), [bash("git *")]);
    assert_eq!(rules("  npm run *  "), [bash("npm run *")]);
    assert!(rules("   ").is_empty());
    assert_eq!(
        rules("*"),
        [PermissionRule {
            tool: "Bash".to_owned(),
            content: None,
        }]
    );
}
