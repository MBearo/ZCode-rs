//! 工具结果内容 ↔ Node tool part 的 `state`（docs/specs/rust-cli/node-store.md §4.6）。
//!
//! 写入：`output` 是 Node `modelMessageContentToText` 的文本；图片照 Node
//! `persistToolResultMediaAttachments` 写成 `state.attachments` 里的 file part，块的顺序记在
//! `state.metadata.modelContentLayout`（Node 冷恢复用同一个字段重建媒体顺序）。内容恰好是一个文本块时
//! 不写 layout，与 Node 的纯文本结果相同。
//!
//! 读取：有合法 layout 时按它重建块；否则 completed 读 `output`，error 读 `metadata.modelContent`
//! （没有时读 `error`），与 Node `hydrateMessageHistoryFromSession` 相同。
use rust_cli_kernel_api::message::{ImageSource, ToolResultContent, tool_reference_text};
use rust_cli_kernel_api::store::ArtifactRef;
use serde_json::{Value, json};

use crate::artifacts;
use crate::notices::RUST_CLI;

/// 写进 tool part 的结果内容。
pub struct Stored {
    pub output: String,
    pub layout: Option<Value>,
    pub attachments: Vec<Value>,
}

/// Node `modelMessageContentBlockToText` 的图片占位（没有 placeholder 名）。
fn placeholder(media_type: &str) -> String {
    format!("[Attached {media_type}]")
}

/// 一张图片 → Node `FilePart`（键序同 Node）。内联图片的 url 是 data URL（Node 没有 artifact store
/// 时的写法）；Artifact 引用的 url 是 `zcode-artifact://`，引用本身记在 `metadata.rustCli`。
pub(crate) fn file_part(
    id: String,
    (session, message): (&str, &str),
    source: &ImageSource,
) -> Value {
    let mut out = json!({"id": id, "messageID": message, "sessionID": session, "type": "file"});
    match source {
        ImageSource::Base64 { media_type, data } => {
            out["mime"] = media_type.as_str().into();
            out["url"] = format!("data:{media_type};base64,{data}").into();
            out["metadata"] = json!({"recoverability": "provider_ready", "storageKind": "inline"});
        }
        ImageSource::Artifact {
            media_type,
            artifact,
        } => {
            let uri = artifacts::uri(artifact);
            out["mime"] = media_type.as_str().into();
            out["url"] = uri.as_str().into();
            out["metadata"] = json!({"artifactUri": uri, "storageKind": "artifact",
                "sizeBytes": artifact.size, RUST_CLI: {"artifact": artifact}});
        }
    }
    out
}

pub fn store(
    content: &[ToolResultContent],
    owner: (&str, &str),
    mut part_id: impl FnMut() -> String,
) -> Stored {
    let texts: Vec<String> = content
        .iter()
        .map(|block| match block {
            ToolResultContent::Text { text } => text.clone(),
            ToolResultContent::Image { source } | ToolResultContent::Document { source } => {
                placeholder(media_type(source))
            }
            // Node 没有 tool_reference：`output` 里是降级文本，Node 续写时按文本发给模型
            // （docs/specs/rust-cli/tools-more.md §3.6）。
            ToolResultContent::ToolReference { tool_name } => tool_reference_text(tool_name),
        })
        .filter(|text| !text.is_empty())
        .collect();
    let mut attachments = Vec::new();
    let layout = match content {
        [ToolResultContent::Text { .. }] => None,
        _ => Some(Value::Array(
            content
                .iter()
                .map(|block| match block {
                    ToolResultContent::Text { text } => json!({"type": "text", "text": text}),
                    // PDF 与图片同一写法（file part，mime 是 application/pdf；node-store.md §4.6，F24）。
                    ToolResultContent::Image { source }
                    | ToolResultContent::Document { source } => {
                        attachments.push(file_part(part_id(), owner, source));
                        json!({"type": "attachment", "attachmentIndex": attachments.len() - 1})
                    }
                    // Node 的 layout 解析器不认识这一项，会退回 `output`；本工程按 layout 还原。
                    ToolResultContent::ToolReference { tool_name } => {
                        json!({"type": "tool_reference", "tool_name": tool_name})
                    }
                })
                .collect(),
        )),
    };
    Stored {
        output: texts.join("\n\n"),
        layout,
        attachments,
    }
}

/// 媒体块在文本里的占位（Node `modelMessageContentBlockToText`，没有文件名时）。
pub(crate) fn attached(source: &ImageSource) -> String {
    placeholder(media_type(source))
}

fn media_type(source: &ImageSource) -> &str {
    match source {
        ImageSource::Base64 { media_type, .. } | ImageSource::Artifact { media_type, .. } => {
            media_type
        }
    }
}

/// Node `isPdfMime`：去掉 `;` 之后的参数，不分大小写。
fn is_pdf(mime: &str) -> bool {
    mime.split(';')
        .next()
        .is_some_and(|base| base.trim().eq_ignore_ascii_case("application/pdf"))
}

/// Node `filePartToContentBlock` 的图片与 PDF 分支，加上本工程的 Artifact 引用。读不出时是 Node 的
/// 占位文本（Node 写的 artifact 媒体要读文件才能还原，冷读取不读 artifact）。
///
/// 修 bug（F24）：以前只认图片，Read 整读 PDF 的 document 块（`application/pdf` 的 data URL）冷读取后变成
/// `[Attached application/pdf: data:…]` 文字，续写时模型看不到文档；现在与 Node `isPdfMime` 分支一样还原。
pub(crate) fn from_file_part(part: &Value) -> ToolResultContent {
    let mime = part["mime"].as_str().unwrap_or("");
    let media = |source: ImageSource| {
        if is_pdf(mime) {
            ToolResultContent::Document { source }
        } else {
            ToolResultContent::Image { source }
        }
    };
    let artifact =
        serde_json::from_value::<ArtifactRef>(part["metadata"][RUST_CLI]["artifact"].clone());
    if let Ok(artifact) = artifact {
        return media(ImageSource::Artifact {
            media_type: mime.to_owned(),
            artifact,
        });
    }
    let url = part["url"].as_str().unwrap_or("");
    let inline = url
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(','))
        .and_then(|(header, data)| Some((header.strip_suffix(";base64")?, data)))
        .filter(|(_, data)| !data.is_empty());
    if let Some((_, data)) = inline
        && is_pdf(mime)
    {
        return media(ImageSource::Base64 {
            media_type: "application/pdf".to_owned(),
            data: data.to_owned(),
        });
    }
    if let Some((header, data)) = inline
        && (mime == "image/*" || mime.starts_with("image/"))
    {
        let media_type = if mime == "image/*" { header } else { mime };
        return ToolResultContent::Image {
            source: ImageSource::Base64 {
                media_type: media_type.to_owned(),
                data: data.to_owned(),
            },
        };
    }
    let label = part["filename"].as_str().unwrap_or(url);
    ToolResultContent::Text {
        text: format!("[Attached {mime}: {label}]"),
    }
}

/// Node `parsePersistedToolMediaLayout` 的条目；本工程的空结果写空数组，也接受。
fn layout(state: &Value) -> Option<Vec<ToolResultContent>> {
    let entries = state["metadata"]["modelContentLayout"].as_array()?;
    let attachments = state["attachments"].as_array();
    entries
        .iter()
        .map(|entry| match entry["type"].as_str()? {
            "text" => Some(ToolResultContent::Text {
                text: entry["text"].as_str()?.to_owned(),
            }),
            "attachment" => {
                let index = usize::try_from(entry["attachmentIndex"].as_u64()?).ok()?;
                attachments?.get(index).map(from_file_part)
            }
            "tool_reference" => Some(ToolResultContent::ToolReference {
                tool_name: entry["tool_name"].as_str()?.to_owned(),
            }),
            _ => None,
        })
        .collect()
}

/// 已结束的 tool part 的结果内容。
pub fn restore(state: &Value) -> Vec<ToolResultContent> {
    if let Some(content) = layout(state) {
        return content;
    }
    let text = if state["status"] == "error" {
        state["metadata"]["modelContent"]
            .as_str()
            .or_else(|| state["error"].as_str())
    } else {
        state["output"].as_str()
    };
    vec![ToolResultContent::Text {
        text: text.unwrap_or("").to_owned(),
    }]
}
