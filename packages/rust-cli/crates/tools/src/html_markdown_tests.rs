use super::*;

#[test]
fn headings_paragraphs_links_and_lists_convert_like_turndown() {
    let html = r#"<html><head><title>T</title><style>p{}</style></head><body>
<h1>Title</h1><p>Hello <strong>bold</strong> and <em>it</em> &amp; <a href="https://example.com/x">a link</a>.</p>
<h3>Sub</h3><ul><li>one</li><li>two</li></ul><ol><li>first</li></ol>
<script>alert(1)</script><pre><code>let x = 1;
let y = 2;</code></pre><p>Use <code>cargo</code>.</p></body></html>"#;
    assert_eq!(
        to_markdown(html),
        "Title\n=====\n\nHello **bold** and _it_ & [a link](https://example.com/x).\n\n### Sub\n\n*   one\n*   two\n\n1.  first\n\n    let x = 1;\n    let y = 2;\n\nUse `cargo`."
    );
}

#[test]
fn entities_comments_and_removed_elements() {
    assert_eq!(
        decode_entities("a&lt;b&#62;c&#x26;d&nbsp;e&unknown;"),
        "a<b>c&d e&unknown;"
    );
    assert_eq!(
        to_markdown("<div>x<!-- hidden --><noscript>no</noscript><iframe src=a></iframe>y</div>"),
        "xy"
    );
    assert_eq!(
        to_markdown("<blockquote><p>quoted</p></blockquote><hr><img src='a.png' alt=pic>"),
        "> quoted\n\n* * *\n\n![pic](a.png)"
    );
}
