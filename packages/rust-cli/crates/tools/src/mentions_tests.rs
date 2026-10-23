use super::*;

#[test]
fn mentions_follow_the_mention_regexes() {
    let input = r#"Look at @a.txt and @many.txt#L3-5 and @dir and @"sp ace.txt" and @missing.txt now, also @a.txt again, @empty.txt, @many.txt#L7, a@b.txt and @dir/inner.txt#intro"#;
    assert_eq!(
        extract(input),
        [
            "sp ace.txt",
            "a.txt",
            "many.txt#L3-5",
            "dir",
            "missing.txt",
            "empty.txt",
            "many.txt#L7",
            "dir/inner.txt#intro",
        ]
    );
}

#[test]
fn trailing_punctuation_backs_off_to_a_word_boundary() {
    assert_eq!(extract("see @dir/."), ["dir"]);
    assert_eq!(extract("@a@b c"), ["a@b"]);
    // 没有 ASCII 单词边界：JS 的 `\b` 匹配不上（不展开）。
    assert!(extract("@文件").is_empty());
    assert_eq!(extract("@文件.md"), ["文件.md"]);
    assert!(extract("mail@example.com").is_empty());
}

#[test]
fn quoted_agent_mentions_are_skipped() {
    assert_eq!(
        extract(r#"ask @"code-reviewer (agent)" about @"x y""#),
        ["x y"]
    );
    assert!(extract(r#"@"" empty"#).is_empty());
}

#[test]
fn line_ranges_become_offset_and_limit() {
    let range = parse_lines("many.txt#L3-5");
    assert_eq!(
        (range.filename.as_str(), range.offset, range.limit),
        ("many.txt", Some(3), Some(3))
    );
    let single = parse_lines("many.txt#L7");
    assert_eq!((single.offset, single.limit), (Some(7), Some(1)));
    let heading = parse_lines("doc.md#intro");
    assert_eq!(
        (heading.filename.as_str(), heading.offset, heading.limit),
        ("doc.md", None, None)
    );
    assert_eq!(parse_lines("a#b#c").filename, "a#b#c");
    assert_eq!(parse_lines("#L1").filename, "#L1");
    // `#L0`：0 不是行号（JS 的 `lineEnd && lineStart` 为假），只给 offset。
    let zero = parse_lines("f#L0");
    assert_eq!((zero.offset, zero.limit), (Some(0), None));
}

#[test]
fn shell_quotes_only_unsafe_paths() {
    assert_eq!(shell_quote("/tmp/w/dir"), "/tmp/w/dir");
    assert_eq!(shell_quote("/tmp/my dir"), "'/tmp/my dir'");
    assert_eq!(shell_quote("it's"), "'it'\"'\"'s'");
    assert_eq!(shell_quote(""), "''");
}

#[test]
fn attachment_texts_are_verbatim() {
    let read = file("/w/a.txt", "a.txt", "1\talpha\n2\tbeta\n3\t");
    assert_eq!(read.source, "file");
    assert_eq!(
        read.text,
        "<system-reminder>\nCalled the Read tool with the following input: {\"file_path\":\"/w/a.txt\"}\nResult of calling the Read tool:\n1\talpha\n2\tbeta\n3\t\n</system-reminder>"
    );
    let listed = directory("/w/dir", "dir", &["inner.txt".to_owned(), "sub".to_owned()]);
    assert!(listed.text.contains(
        "{\"command\":\"ls /w/dir\",\"description\":\"Lists files in /w/dir\"}\nResult of calling the Bash tool:\ninner.txt\nsub\n"
    ));
    let big = too_large("/w/<big>.txt", "290KB");
    assert!(
        big.text
            .contains("The user @-mentioned /w/&lt;big&gt;.txt (290KB). Its contents")
    );
    assert_eq!(big.source, "at_mention_reference");
}

#[test]
fn directory_listings_are_capped() {
    let names: Vec<String> = (0..1_005).map(|i| format!("f{i:04}")).collect();
    let listed = directory_names(names);
    assert_eq!(listed.len(), 1_001);
    assert_eq!(
        listed.last().map(String::as_str),
        Some("\u{2026} and 5 more entries")
    );
    assert_eq!(directory_names(vec!["b".into(), "a".into()]), ["a", "b"]);
}
