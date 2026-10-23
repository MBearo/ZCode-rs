use super::*;

fn globs_of(raw: &str) -> Option<Vec<String>> {
    split(raw).1
}

#[test]
fn text_without_frontmatter_is_kept_as_is() {
    let raw = "\u{feff}# Rules\nbody\n";
    assert_eq!(split(raw), (raw.to_owned(), None));
    // 只有开头的 `---`，后面没有第二个：不是 frontmatter。
    assert_eq!(split("---\nbody\n").0, "---\nbody\n");
}

#[test]
fn frontmatter_is_removed_and_paths_become_globs() {
    let (content, globs) = split("---\npaths: src/**/*.ts\n---\n# TS rules\n");
    assert_eq!(content, "# TS rules\n");
    assert_eq!(globs, Some(vec!["src/**/*.ts".to_owned()]));
    // 没有 paths 的 frontmatter 也去掉，规则是无条件的。
    assert_eq!(
        split("---\ndescription: x\n---\nbody"),
        ("body".to_owned(), None)
    );
    // BOM 不影响匹配，CRLF 的分隔行也算（`\s*` 吃掉 `\r`）。
    assert_eq!(split("\u{feff}---\r\npaths: a\r\n---\r\nbody").0, "body");
}

#[test]
fn paths_split_on_top_level_commas_and_expand_braces() {
    assert_eq!(
        globs_of("---\npaths: \"src/*.{ts,tsx}, docs/**\"\n---\n"),
        Some(vec![
            "src/*.ts".to_owned(),
            "src/*.tsx".to_owned(),
            "docs".to_owned()
        ])
    );
    assert_eq!(
        globs_of("---\npaths:\n  - \"{a,b}/{c,d}\"\n  - lib/x.rs # note\n---\n"),
        Some(
            ["a/c", "a/d", "b/c", "b/d", "lib/x.rs"]
                .map(str::to_owned)
                .to_vec()
        )
    );
    assert_eq!(
        globs_of("---\npaths: [src/a.ts, 'b c.md']\n---\n"),
        Some(vec!["src/a.ts".to_owned(), "b c.md".to_owned()])
    );
}

#[test]
fn match_all_and_empty_paths_mean_no_condition() {
    assert_eq!(globs_of("---\npaths: \"**\"\n---\n"), None);
    assert_eq!(globs_of("---\npaths: \"**, /**\"\n---\n"), None);
    assert_eq!(globs_of("---\npaths: \"\"\n---\n"), None);
    assert_eq!(globs_of("---\npaths:\n---\n"), None);
}

#[test]
fn unquoted_globs_follow_the_yaml_fallbacks() {
    // `paths: **/*.ts`：YAML 把 `*` 当别名报错，给整行加引号重试。
    assert_eq!(
        globs_of("---\npaths: **/*.ts\n---\n"),
        Some(vec!["**/*.ts".to_owned()])
    );
    // 块列表项无法加引号重试：整个 frontmatter 视为空，规则变成无条件。
    assert_eq!(globs_of("---\npaths:\n  - **/*.ts\n---\n"), None);
}
