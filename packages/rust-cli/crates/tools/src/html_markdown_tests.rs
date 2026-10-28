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

/// 修复线 F109：`&` 之后 12 个字节以内有多字节字符时，以前先按字节切出这 12 个字节，切点落在字符中间就 panic。
/// 现在认得出的实体照常解码，认不出的保留原文。
#[test]
fn entities_next_to_multibyte_text_decode_without_panicking() {
    // 用户页面里的原文：`&nbsp;` 加 4 个 ASCII 字节之后，“显”占第 10..13 个字节。
    let page = "&nbsp;Max 显示屏采用曲线优美的圆角设计，四个圆角位于一个标准矩形内。";
    assert_eq!(
        decode_entities(page),
        " Max 显示屏采用曲线优美的圆角设计，四个圆角位于一个标准矩形内。"
    );
    // `&` 后面直接是中文、没有分号；分号在 12 个字节之外：都保留原文。
    assert_eq!(decode_entities("&中文实体没有分号"), "&中文实体没有分号");
    assert_eq!(decode_entities("a & 中文说明文字"), "a & 中文说明文字");
    assert_eq!(decode_entities("&中文中文;"), "&中文中文;");
    // 实体之后紧跟中文：照常解码。
    assert_eq!(decode_entities("&amp;中文内容"), "&中文内容");
    assert_eq!(decode_entities("&#x4E2D;文字"), "中文字");
    // `&` 在末尾。
    assert_eq!(decode_entities("结尾&"), "结尾&");
    assert_eq!(
        to_markdown("<p>&nbsp;Max 显示屏采用曲线优美的圆角设计</p>"),
        "Max 显示屏采用曲线优美的圆角设计"
    );
}
