//! 结果预算与落盘（tools-files.md §5）。
//!
//! 工具结果的文本超过阈值（工具声明的上限与 50000 取小）时，整份内容经 [`ArtifactStore`] 落盘，
//! 交给模型的是 `<persisted-output>` 预览：落盘位置、原大小与前 2KB。结果为空时换成
//! `(<工具名> completed with no output)`。含图片的结果不落盘。落盘与预览只经 kernel-api 的
//! [`persisted_output`]（hook 的大输出走同一入口，hooks.md §4.5）。
//!
//! 落盘位置不是本机路径，而是 `zcode-artifact://<会话>/<artifact>`
//! （云端也能用）；Read 能读这种地址（tools-files.md §5）。

use rust_cli_base::proto::mime;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::message::ToolResultContent;
use rust_cli_kernel_api::persisted_output;
use rust_cli_kernel_api::store::ArtifactStore;
use rust_cli_kernel_api::tool::ToolOutput;
use serde_json::{Value, json};

use crate::common::utf16_len;

pub use rust_cli_kernel_api::persisted_output::ARTIFACT_SCHEME;
pub(crate) use rust_cli_kernel_api::persisted_output::{message as persisted_message, preview};

/// 所有工具共用的上限。
const DEFAULT_MAX_CHARS: usize = 50_000;

fn texts(output: &ToolOutput) -> Option<Vec<&str>> {
    output
        .content
        .iter()
        .map(|block| match block {
            ToolResultContent::Text { text } => Some(text.as_str()),
            ToolResultContent::Image { .. }
            | ToolResultContent::Document { .. }
            | ToolResultContent::ToolReference { .. } => None,
        })
        .collect()
}

/// 按预算处理一个成功的工具结果。`declared` 是工具的 `max_result_size_chars`（None 不落盘）。
pub async fn apply(
    output: ToolOutput,
    tool: &str,
    declared: Option<usize>,
    session: &SessionId,
    artifacts: &dyn ArtifactStore,
) -> ToolOutput {
    let Some(parts) = texts(&output) else {
        return output;
    };
    if parts.iter().all(|part| part.trim().is_empty()) {
        return ToolOutput {
            content: vec![ToolResultContent::Text {
                text: format!("({tool} completed with no output)"),
            }],
            ..output
        };
    }
    let Some(limit) = declared.map(|declared| declared.min(DEFAULT_MAX_CHARS)) else {
        return output;
    };
    let size: usize = parts.iter().map(|part| utf16_len(part)).sum();
    if size <= limit {
        return output;
    }
    // 单段文本存原文；多段存 JSON（`JSON.stringify(blocks, null, 2)` 的写法）。
    let (body, media_type) = match parts.as_slice() {
        [only] => ((*only).to_owned(), mime::TEXT_PLAIN),
        many => {
            let blocks: Vec<Value> = many
                .iter()
                .map(|text| json!({"type": "text", "text": text}))
                .collect();
            let text = serde_json::to_string_pretty(&blocks).unwrap_or_default();
            (text, mime::JSON)
        }
    };
    let Ok(text) = persisted_output::persist(artifacts, session, &body, media_type).await else {
        // 存不下时原样返回。
        return output;
    };
    ToolOutput {
        content: vec![ToolResultContent::Text { text }],
        ..output
    }
}

#[cfg(test)]
#[path = "budget_tests.rs"]
mod tests;
