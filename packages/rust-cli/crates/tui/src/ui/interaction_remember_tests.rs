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

#[test]
fn several_rules_are_listed_with_the_directory() {
    let two = [bash("mytool a *"), bash("othertool b *")];
    assert_eq!(
        commands_label(&two, "/w", Lang::En),
        "Yes, and don't ask again for mytool a and othertool b commands in /w"
    );
    let three = [bash("a *"), bash("b:*"), bash("c d")];
    assert_eq!(
        commands_label(&three, "/w", Lang::En),
        "Yes, and don't ask again for a, b, and c d commands in /w"
    );
    assert_eq!(
        commands_label(&three, "/w", Lang::Zh),
        "是，以后在 /w 里 a、b 和 c d 命令不再询问"
    );
}
