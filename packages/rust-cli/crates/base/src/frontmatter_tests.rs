use super::{Value, split};

#[test]
fn parses_scalars_lists_and_block_text() {
    let (map, body) = split(
        "---\nname: reviewer\ndescription: \"Reviews code.\"\ntools: Read, Grep\ndisallowedTools:\n  - Bash\n  - 'Write'\nmodel: inherit\nnotes: |\n  line one\n  line two\nfolded: >\n  one\n  two\nlist: [a, \"b\", ]\n---\n\nYou review code.\n",
    )
    .unwrap();
    assert_eq!(map["name"], Value::Text("reviewer".to_owned()));
    assert_eq!(map["description"], Value::Text("Reviews code.".to_owned()));
    assert_eq!(map["tools"], Value::Text("Read, Grep".to_owned()));
    assert_eq!(
        map["disallowedTools"],
        Value::List(vec!["Bash".to_owned(), "Write".to_owned()])
    );
    assert_eq!(map["notes"], Value::Text("line one\nline two".to_owned()));
    assert_eq!(map["folded"], Value::Text("one two".to_owned()));
    assert_eq!(
        map["list"],
        Value::List(vec!["a".to_owned(), "b".to_owned()])
    );
    assert_eq!(body, "\nYou review code.\n");
}

/// 键后面空着、下面是缩进的非列表行：嵌套块的原文；空的嵌套块与注释不算键。
#[test]
fn nested_objects_keep_their_text_as_a_block() {
    let (map, _) = split(
        "---\nhooks:\n  PreToolUse:\n    matcher: Bash   \nempty:\n\n# comment: no\nname: x\nsteps:\n  first:\n    - a\n---\n",
    )
    .unwrap();
    assert_eq!(
        map["hooks"],
        Value::Block("  PreToolUse:\n    matcher: Bash".to_owned())
    );
    // 有 `- ` 行时是列表（只取这些行；不是完整的 YAML）。
    assert_eq!(map["steps"], Value::List(vec!["a".to_owned()]));
    assert_eq!(map["hooks"].text(), None);
    assert!(!map.contains_key("empty"));
    assert!(!map.contains_key("# comment"));
    assert_eq!(map["name"].text(), Some("x"));
}

#[test]
fn files_without_a_closed_frontmatter_have_none() {
    assert!(split("# Just a note\n").is_none());
    assert!(split("---\nname: unterminated\n").is_none());
    let (map, body) = split("\u{feff}---\r\nname: x\r\n---\r\nbody").unwrap();
    assert_eq!(map["name"], Value::Text("x".to_owned()));
    assert_eq!(body, "body");
    // 结尾的 `---` 没有换行也算。
    let (map, body) = split("---\nname: y\n---").unwrap();
    assert_eq!(map["name"], Value::Text("y".to_owned()));
    assert_eq!(body, "");
}
