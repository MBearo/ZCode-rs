//! MCP 工具结果的转换与截断（docs/specs/rust-cli/ext.md §3.5）。

use rust_cli_kernel_api::message::{ImageSource, ToolResultContent};
use serde_json::Value;

/// 图片按多少 token 估算。
const IMAGE_TOKENS: u64 = 1600;

/// `isError: true` 的错误文本：第一个内容块的 `text`，没有时 `Unknown error`。
pub(crate) fn error_text(result: &Value) -> Option<String> {
    if result.get("isError").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let first = result
        .get("content")
        .and_then(Value::as_array)
        .and_then(|content| content.first());
    let text = match first {
        Some(block) => block.get("text").and_then(Value::as_str).map(str::to_owned),
        None => result.get("error").map(|error| match error {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        }),
    };
    Some(text.unwrap_or_else(|| "Unknown error".to_owned()))
}

fn text(text: String) -> ToolResultContent {
    ToolResultContent::Text { text }
}

fn image(data: &str, mime: Option<&str>) -> ToolResultContent {
    let subtype = mime
        .and_then(|mime| mime.split('/').nth(1))
        .filter(|subtype| !subtype.is_empty())
        .unwrap_or("png");
    ToolResultContent::Image {
        source: ImageSource::Base64 {
            media_type: format!("image/{subtype}"),
            data: data.to_owned(),
        },
    }
}

fn binary(prefix: &str, mime: Option<&str>, data: &str) -> ToolResultContent {
    // 二进制不写成文件、不给路径（ext.md §8 D9）。
    let bytes = data.len() / 4 * 3;
    text(format!(
        "{prefix}Binary content ({}, {bytes} bytes) could not be saved to disk: not supported",
        mime.unwrap_or("unknown type")
    ))
}

/// 一个内容块（`transformResultContent`）。
fn block(item: &Value, server: &str) -> Vec<ToolResultContent> {
    let field = |key: &str| item.get(key).and_then(Value::as_str);
    match field("type") {
        Some("text") => vec![text(field("text").unwrap_or_default().to_owned())],
        Some("image") => vec![image(field("data").unwrap_or_default(), field("mimeType"))],
        Some("audio") => vec![binary(
            &format!("[Audio from {server}] "),
            field("mimeType"),
            field("data").unwrap_or_default(),
        )],
        Some("resource") => {
            let Some(resource) = item.get("resource") else {
                return Vec::new();
            };
            let get = |key: &str| resource.get(key).and_then(Value::as_str);
            let prefix = format!(
                "[Resource from {server} at {}] ",
                get("uri").unwrap_or_default()
            );
            if let Some(body) = get("text") {
                return vec![text(format!("{prefix}{body}"))];
            }
            let Some(blob) = get("blob") else {
                return Vec::new();
            };
            let mime = get("mimeType");
            if mime.is_some_and(|mime| mime.starts_with("image/")) {
                vec![text(prefix), image(blob, mime)]
            } else {
                vec![binary(&prefix, mime, blob)]
            }
        }
        Some("resource_link") => {
            let mut line = format!(
                "[Resource link: {}] {}",
                field("name").unwrap_or_default(),
                field("uri").unwrap_or_default()
            );
            if let Some(description) = field("description").filter(|text| !text.is_empty()) {
                line.push_str(&format!(" ({description})"));
            }
            vec![text(line)]
        }
        _ => Vec::new(),
    }
}

/// 成功结果的内容；形状不对时为 None（报 `unexpected response format`）。
pub(crate) fn transform(result: &Value, server: &str) -> Option<Vec<ToolResultContent>> {
    if let Some(value) = result.get("toolResult") {
        let rendered = match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        return Some(vec![text(rendered)]);
    }
    if let Some(structured) = result
        .get("structuredContent")
        .filter(|value| !value.is_null())
    {
        return Some(vec![text(structured.to_string())]);
    }
    let content = result.get("content")?.as_array()?;
    Some(
        content
            .iter()
            .flat_map(|item| block(item, server))
            .collect(),
    )
}

fn estimate(content: &[ToolResultContent]) -> u64 {
    content
        .iter()
        .map(|item| match item {
            ToolResultContent::Text { text } => (text.chars().count() as u64).div_ceil(4),
            ToolResultContent::Image { .. } | ToolResultContent::Document { .. } => IMAGE_TOKENS,
            // MCP 的结果里没有 tool_reference（只有 ToolSearch 产生）。
            ToolResultContent::ToolReference { .. } => 0,
        })
        .sum()
}

/// 截断说明。
fn truncation_message(max_tokens: u64) -> String {
    format!(
        "\n\n[OUTPUT TRUNCATED - exceeded {max_tokens} token limit]\n\nThe tool output was truncated. If this MCP server provides pagination or filtering tools, use them to retrieve specific portions of the data. If pagination is not available, inform the user that you are working with truncated output and results may be incomplete."
    )
}

/// 超过上限时按 `max_tokens × 4` 个字符截断文本，末尾加说明（ext.md §3.5，§8 D8）。
pub(crate) fn truncate(content: Vec<ToolResultContent>, max_tokens: u64) -> Vec<ToolResultContent> {
    // 没有 count_tokens，不做精确计数：按估算直接比较上限。
    if estimate(&content) <= max_tokens {
        return content;
    }
    let mut left = usize::try_from(max_tokens.saturating_mul(4)).unwrap_or(usize::MAX);
    let mut out = Vec::new();
    for item in content {
        match item {
            ToolResultContent::Text { text } => {
                if left == 0 {
                    break;
                }
                let kept: String = text.chars().take(left).collect();
                left -= kept.chars().count();
                out.push(ToolResultContent::Text { text: kept });
            }
            image @ (ToolResultContent::Image { .. } | ToolResultContent::Document { .. }) => {
                let cost = usize::try_from(IMAGE_TOKENS * 4).unwrap_or(usize::MAX);
                if cost <= left {
                    left -= cost;
                    out.push(image);
                }
            }
            reference @ ToolResultContent::ToolReference { .. } => out.push(reference),
        }
    }
    out.push(ToolResultContent::Text {
        text: truncation_message(max_tokens),
    });
    out
}

#[cfg(test)]
#[path = "result_tests.rs"]
mod tests;
