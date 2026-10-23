//! 参数替换（ext.md §2.5）。

use super::*;

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| (*name).to_owned()).collect()
}

#[test]
fn k1_greet_substitutes_arguments_named_and_indexed() {
    let body = "Say hello to $ARGUMENTS ($who, first: $0).\n";
    assert_eq!(
        substitute(body, "Ada", &names(&["who"])),
        "Say hello to Ada (Ada, first: Ada).\n"
    );
}

#[test]
fn k1_plain_appends_the_arguments_when_there_is_no_placeholder() {
    let body = "Base directory for this skill: /w/.zcode/skills/plain\n\n# Plain skill\n\nNo frontmatter here.\n";
    assert_eq!(
        substitute(body, "extra words", &[]),
        format!("{body}\n\nARGUMENTS: extra words")
    );
    assert_eq!(substitute("No args.", "", &[]), "No args.");
}

#[test]
fn shell_words_respect_quotes() {
    assert_eq!(
        split_args(r#"one "two words" 'three' four\ five"#),
        ["one", "two words", "three", "four five"]
    );
    assert_eq!(
        substitute("$1|$ARGUMENTS[0]|$9|$ARGUMENTS[9]", r#"a "b c""#, &[]),
        "b c|a|$9|$ARGUMENTS[9]"
    );
}

#[test]
fn escapes_and_word_boundaries() {
    // `\$` 转义不替换；`$whoami` 不是 `` $who ``；`` $0x `` 不是 `` $0 ``。
    assert_eq!(
        substitute(r"\$ARGUMENTS $whoami $who $0x", "Ada", &names(&["who"])),
        r"\$ARGUMENTS $whoami Ada $0x".replacen(r"\$ARGUMENTS", "$ARGUMENTS", 1)
    );
    // 替换进去的值里的 `` $ `` 不会再被替换。
    assert_eq!(
        substitute("[$ARGUMENTS]", "$0 $ARGUMENTS", &[]),
        "[$0 $ARGUMENTS]"
    );
    // 长名字优先：`$name` 与 `$names`。
    assert_eq!(
        substitute("$names/$name", "x y", &names(&["name", "names"])),
        "y/x"
    );
}

#[test]
fn substituted_values_cannot_inject_inline_commands() {
    assert_eq!(
        substitute("Run: $ARGUMENTS", "!`rm -rf /`", &[]),
        r"Run: \! `rm -rf /`"
    );
    assert_eq!(substitute("x $ARGUMENTS", "a`!b", &[]), r"x a` \!b");
}
