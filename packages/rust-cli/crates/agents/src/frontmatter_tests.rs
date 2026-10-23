use super::{Value, split, tools};

#[test]
fn parses_scalars_lists_and_block_text() {
    let (map, body) = split(
        "---\nname: reviewer\ndescription: \"Reviews code.\"\ntools: Read, Grep\ndisallowedTools:\n  - Bash\n  - 'Write'\nmodel: inherit\nnotes: |\n  line one\n  line two\n---\n\nYou review code.\n",
    )
    .unwrap();
    assert_eq!(map["name"], Value::Text("reviewer".to_owned()));
    assert_eq!(map["description"], Value::Text("Reviews code.".to_owned()));
    assert_eq!(
        map["disallowedTools"],
        Value::List(vec!["Bash".to_owned(), "Write".to_owned()])
    );
    assert_eq!(map["notes"], Value::Text("line one\nline two".to_owned()));
    assert_eq!(body, "\nYou review code.\n");
    assert_eq!(
        tools(map.get("tools")),
        Some(vec!["Read".to_owned(), "Grep".to_owned()])
    );
}

#[test]
fn star_or_missing_tools_mean_all_tools() {
    let (map, _) = split("---\nname: a\ntools: [Read, \"*\"]\n---\n").unwrap();
    assert_eq!(tools(map.get("tools")), None);
    assert_eq!(tools(None), None);
}

#[test]
fn files_without_frontmatter_are_not_agents() {
    assert!(split("# Just a note\n").is_none());
    assert!(split("---\nname: unterminated\n").is_none());
    let (map, body) = split("\u{feff}---\r\nname: x\r\n---\r\nbody").unwrap();
    assert_eq!(map["name"], Value::Text("x".to_owned()));
    assert_eq!(body, "body");
}
