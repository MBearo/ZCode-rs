use super::*;

#[test]
fn block_comments_are_removed_with_the_blank_lines_after_them() {
    let text = "# Title\n\n<!-- note for maintainers -->\n\nBody\n";
    assert_eq!(strip_comments(text), "# Title\n\nBody\n");
    // 多行注释，注释后同一行的文字保留。
    let text = "a\n\n<!--\nhidden\n--> kept\nb\n";
    assert_eq!(strip_comments(text), "a\n\n kept\nb\n");
}

#[test]
fn comments_in_code_inline_or_unclosed_are_kept() {
    let fenced = "```\n<!-- in code -->\n```\n";
    assert_eq!(strip_comments(fenced), fenced);
    let inline = "text <!-- inline --> more\n";
    assert_eq!(strip_comments(inline), inline);
    let unclosed = "a\n\n<!-- never closed\nrest\n";
    assert_eq!(strip_comments(unclosed), unclosed);
    let indented = "    <!-- indented code -->\n";
    assert_eq!(strip_comments(indented), indented);
}

#[test]
fn includes_are_found_in_text_only() {
    let text = "\
See @./docs/a.md and @b.md#section.
Home @~/notes/x.md, absolute @/etc/zcode/c.md.
Escaped @dir/with\\ space.md
`@code.md` and @@twice.md and email dev@example.com.

```
@fenced.md
```

    @indented.md

<!-- @hidden.md --> @after-comment.md
";
    assert_eq!(
        includes(text),
        [
            "./docs/a.md",
            "b.md",
            // 标点紧跟时算进路径。
            "~/notes/x.md,",
            "/etc/zcode/c.md.",
            "dir/with space.md",
            "after-comment.md",
        ]
    );
}

#[test]
fn includes_follow_the_accepted_shapes() {
    assert_eq!(includes("@/ alone"), Vec::<String>::new());
    assert_eq!(includes("@#tag @%x @(y)"), Vec::<String>::new());
    assert_eq!(includes("@a.md @a.md"), ["a.md"]);
    // 行内代码以同样长度的反引号闭合；HTML 标签之后的 @ 仍是文本的开头。
    assert_eq!(includes("``x ` @in-code.md``<b>@tag.md</b>"), ["tag.md"]);
    // 段落之间的反引号不配对。
    assert_eq!(includes("a `b\n\n@c.md `d"), ["c.md"]);
}
