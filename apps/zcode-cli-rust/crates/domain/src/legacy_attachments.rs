//! Legacy `session/send` attachments (Host `ZCodePromptAttachment` objects) as
//! V4 attachment inputs, following Node `mapProtocolPromptAttachment`: items
//! Node would drop are dropped, never rejected.
use crate::attachment_upload::valid_mime;
use base64::Engine as _;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig, general_purpose};
use serde_json::Value;

/// Node `decodeText`: inline file bytes are only read up to this size.
const INLINE_FILE_BYTES: u64 = 65_536;

/// Where the attachment's bytes come from.
#[derive(Debug, PartialEq)]
pub enum Source {
    /// A local path or `file://` URL the admission snapshots.
    Path(String),
    /// Inline content the runtime stores as a session artifact first.
    Bytes(Vec<u8>),
}

#[derive(Debug, PartialEq)]
pub struct Mapped {
    pub file_name: String,
    pub mime: String,
    pub source: Source,
}

#[derive(Clone, Copy, PartialEq)]
enum Category {
    Pdf,
    Image,
    Video,
    File,
    Audio,
}

/// `mimeType` without parameters, lowercased.
fn base_mime(item: &Value) -> Option<String> {
    let raw = item["mimeType"].as_str()?;
    let base = raw
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    (!base.is_empty()).then_some(base)
}

/// Lenient like Node `Buffer.from(b64, "base64")`: whitespace, missing padding
/// and the URL-safe alphabet are accepted.
fn decode(encoded: &str) -> Option<Vec<u8>> {
    let compact: String = encoded
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    let config =
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent);
    GeneralPurpose::new(&base64::alphabet::STANDARD, config)
        .decode(&compact)
        .or_else(|_| GeneralPurpose::new(&base64::alphabet::URL_SAFE, config).decode(&compact))
        .or_else(|_| general_purpose::STANDARD.decode(&compact))
        .ok()
}

/// Image type from the leading bytes or, for paths, the extension.
fn image_mime(bytes: Option<&[u8]>, name: &str) -> Option<&'static str> {
    if let Some(b) = bytes {
        let sniffed = if b.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some("image/png")
        } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
            Some("image/jpeg")
        } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
            Some("image/gif")
        } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
            Some("image/webp")
        } else {
            None
        };
        if sniffed.is_some() {
            return sniffed;
        }
    }
    let extension = name.rsplit_once('.')?.1.to_ascii_lowercase();
    match extension.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// V4 `fileName`: no NUL or line breaks, at most 255 characters.
fn file_name(item: &Value) -> String {
    let raw = item["filename"].as_str().unwrap_or("attachment");
    let name: String = raw
        .chars()
        .filter(|c| !matches!(c, '\0' | '\r' | '\n'))
        .take(255)
        .collect();
    if name.is_empty() {
        "attachment".into()
    } else {
        name
    }
}

/// Maps one legacy attachment; `None` where Node drops it.
pub fn map(item: &Value) -> Option<Mapped> {
    let base = base_mime(item);
    let category = match item["kind"].as_str() {
        _ if base.as_deref() == Some("application/pdf") => Category::Pdf,
        Some("pdf") => Category::Pdf,
        Some("image") => Category::Image,
        Some("video") => Category::Video,
        Some("file") => Category::File,
        Some("audio") => Category::Audio,
        _ => return None,
    };
    let name = file_name(item);
    let path = item["localPath"]
        .as_str()
        .filter(|p| !p.is_empty())
        .map(str::to_owned);
    let inline = item["dataBase64"].as_str().filter(|b| !b.is_empty());
    let text = item["textContent"].as_str();
    let source = match (category, path) {
        (_, Some(path)) => Source::Path(path),
        (Category::Pdf | Category::Image | Category::Video, None) => {
            Source::Bytes(decode(inline?)?)
        }
        (Category::File | Category::Audio, None) => match text {
            Some(text) => Source::Bytes(text.as_bytes().to_vec()),
            None => {
                let size = item["sizeBytes"].as_f64();
                if size.is_some_and(|s| s > INLINE_FILE_BYTES as f64) {
                    return None;
                }
                Source::Bytes(decode(inline?)?)
            }
        },
    };
    let valid = base.filter(|b| valid_mime(b));
    let mime = match category {
        Category::Pdf => "application/pdf".to_owned(),
        Category::Image => match valid.filter(|b| b.starts_with("image/")) {
            Some(mime) => mime,
            None => {
                let bytes = match &source {
                    Source::Bytes(bytes) => Some(bytes.as_slice()),
                    Source::Path(_) => None,
                };
                image_mime(bytes, &name)?.to_owned()
            }
        },
        Category::Video => valid.unwrap_or_else(|| "video/mp4".into()),
        // Node 把 file 当文本文件交给运行时：音频 MIME 同样按二进制文件处理。
        Category::File => valid
            .filter(|b| !b.starts_with("audio/"))
            .unwrap_or_else(|| {
                if text.is_some() {
                    "text/plain".into()
                } else {
                    "application/octet-stream".into()
                }
            }),
        // Node 把音频按普通文件交给运行时；V4 不接受 audio/*。
        Category::Audio => "application/octet-stream".into(),
    };
    Some(Mapped {
        file_name: name,
        mime,
        source,
    })
}

#[cfg(test)]
#[path = "legacy_attachments_tests.rs"]
mod tests;
