//! `/context` 的本地估算（docs/specs/rust-cli/context-usage.md §3，工作线 F93）：文字按每 token 字符数、工具按 JSON
//! 长度 / 2 加开销、没有消息时是 0、图片 2000。
use rust_cli_kernel_api::model::ToolDefinition;
use serde_json::json;

use super::{Counter, TOOL_OVERHEAD, rough, user_text};

#[test]
fn text_tools_and_images_follow_the_local_estimate() {
    let counter = Counter { per_token: 3 };
    // `Math.round(长度 / 3)`：9 个字符 3，10 个 3（3.33），11 个 4（3.67）；UTF-16 计长度。
    assert_eq!(counter.text("abcdefghi"), 3);
    assert_eq!(counter.text("abcdefghij"), 3);
    assert_eq!(counter.text("abcdefghijk"), 4);
    assert_eq!(counter.text("é😀"), 1);
    assert_eq!(Counter { per_token: 4 }.text("abcdefghij"), 3);
    let tool = ToolDefinition {
        name: "Read".to_owned(),
        description: "Reads a file.".to_owned(),
        input_schema: json!({"type": "object"}),
        cache_control: None,
    };
    let json = serde_json::to_string(&[&tool]).unwrap();
    assert_eq!(
        counter.tools(std::slice::from_ref(&tool)),
        rough(&json, 2) + TOOL_OVERHEAD
    );
    // 没有消息、没有工具是 0（不发计数请求，也就没有计数请求里的占位工具 `foo`）。
    assert_eq!(counter.count(&[], &[]), 0);
    let mut image = user_text("x");
    image[0].content[0].block = rust_cli_kernel_api::message::ContentBlock::Image {
        source: rust_cli_kernel_api::message::ImageSource::Base64 {
            media_type: "image/png".to_owned(),
            data: "AAAA".to_owned(),
        },
    };
    assert_eq!(counter.count(&image, &[]), rust_cli_compact::IMAGE_TOKENS);
}
