//! MCP 结果的转换与截断（ext.md §3.5）。

use serde_json::json;

use super::*;

fn texts(content: &[ToolResultContent]) -> Vec<String> {
    content
        .iter()
        .map(|item| match item {
            ToolResultContent::Text { text } => text.clone(),
            ToolResultContent::Image { source } => format!("image {source:?}"),
            other => format!("{other:?}"),
        })
        .collect()
}

#[test]
fn errors_take_the_first_text() {
    assert_eq!(
        error_text(&json!({"content": [{"type": "text", "text": "x"}]})),
        None
    );
    assert_eq!(
        error_text(&json!({"isError": true, "content": [{"type": "text", "text": "boom"}]})),
        Some("boom".to_owned())
    );
    assert_eq!(
        error_text(&json!({"isError": true, "content": []})),
        Some("Unknown error".to_owned())
    );
}

#[test]
fn content_blocks_are_converted() {
    let result = json!({"content": [
        {"type": "text", "text": "hello"},
        {"type": "resource", "resource": {"uri": "file:///a.txt", "text": "body"}},
        {"type": "resource_link", "uri": "file:///b", "name": "b", "description": "the b"},
        {"type": "image", "data": "aGk=", "mimeType": "image/jpeg"},
        {"type": "unknown"}
    ]});
    let content = transform(&result, "files").unwrap();
    assert_eq!(
        texts(&content)[..3],
        [
            "hello".to_owned(),
            "[Resource from files at file:///a.txt] body".to_owned(),
            "[Resource link: b] file:///b (the b)".to_owned(),
        ]
    );
    assert!(matches!(
        &content[3],
        ToolResultContent::Image { source: ImageSource::Base64 { media_type, .. } } if media_type == "image/jpeg"
    ));
    assert_eq!(content.len(), 4);
    assert_eq!(
        texts(&transform(&json!({"structuredContent": {"a": 1}, "content": []}), "s").unwrap()),
        ["{\"a\":1}"]
    );
    assert!(transform(&json!({"nothing": true}), "s").is_none());
}

#[test]
fn large_text_is_truncated_with_a_note() {
    let content = vec![ToolResultContent::Text {
        text: "x".repeat(50),
    }];
    assert_eq!(truncate(content.clone(), 100), content);
    let cut = truncate(content, 5);
    let parts = texts(&cut);
    assert_eq!(parts[0], "x".repeat(20));
    assert!(parts[1].starts_with(
        "\n\n[OUTPUT TRUNCATED - exceeded 5 token limit]\n\nThe tool output was truncated."
    ));
}
