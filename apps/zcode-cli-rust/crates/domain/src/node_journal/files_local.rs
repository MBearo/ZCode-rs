//! Node prompt attachments of local path references that are not stored as
//! artifacts: text previews, path references and placeholders
//! (`resolveLocalFileAttachment`, `resolvedPathReferenceAttachment`).
use super::{Local, NodeFile, basename, local_source, part, text_block};
use serde_json::json;

/// A local text file read for the model (Node `readTextFileForModel`).
pub struct TextRead<'a> {
    pub content: &'a str,
    pub truncated: bool,
    /// The file size.
    pub size: u64,
    pub total_lines: u64,
}

/// Node `resolveLocalFileAttachment` of a text file.
pub fn local_text((original, path): Local, read: TextRead) -> NodeFile {
    let meta = json!({"originalUrl": original,
        "preview": {"text": read.content, "truncated": read.truncated, "originalBytes": read.size,
            "startLine": 1, "totalLines": read.total_lines},
        "recoverability": if read.truncated { "preview_only" } else { "provider_ready" },
        "sizeBytes": read.size, "storageKind": "inline"});
    NodeFile {
        part: part(
            "text/plain",
            Some(basename(path)),
            original,
            Some(local_source(original, path)),
            meta,
        ),
        block: text_block(read.content),
    }
}

/// Node `resolvedPathReferenceAttachment`: `reason` is its `PathReferenceReason`.
pub fn local_reference((original, path): Local, mime: &str, size: u64, reason: &str) -> NodeFile {
    let why = match reason {
        "image_too_large" => "the image is larger than the inline media budget",
        "pdf_too_large" => "the PDF is larger than the inline PDF input limit",
        "video_too_large" => "the video is larger than the ZCode video input limit",
        _ => "the file is not a known text attachment",
    };
    let text = format!(
        "Attached {mime}: {original}\nThe file was sent by local path because {why}.\nUse the available file reading tools if you need to inspect the file contents."
    );
    let meta = json!({"originalUrl": original, "recoverability": "metadata_only",
        "sizeBytes": size, "storageKind": "local_ref"});
    NodeFile {
        part: part(
            mime,
            Some(basename(path)),
            original,
            Some(local_source(original, path)),
            meta,
        ),
        block: text_block(&text),
    }
}

/// Node `resolvedPlaceholderAttachment` of a local file that is not sent;
/// `size` is its `sizeBytes` when Node passes one.
pub fn local_failed(
    (original, path): Local,
    mime: &str,
    code: &str,
    size: Option<u64>,
) -> NodeFile {
    let mut meta = serde_json::Map::new();
    meta.insert("errorCode".into(), code.into());
    meta.insert("originalUrl".into(), original.into());
    meta.insert("recoverability".into(), "metadata_only".into());
    if let Some(size) = size {
        meta.insert("sizeBytes".into(), size.into());
    }
    meta.insert("storageKind".into(), "local_ref".into());
    let meta = serde_json::Value::Object(meta);
    NodeFile {
        part: part(
            mime,
            Some(basename(path)),
            original,
            Some(local_source(original, path)),
            meta,
        ),
        block: text_block(&format!("[Attached {mime}: {original}]")),
    }
}

/// Node `isTextLikePath`.
pub fn text_like(path: &str) -> bool {
    const EXTENSIONS: &[&str] = &[
        "cjs", "conf", "cpp", "cs", "css", "csv", "go", "h", "hpp", "html", "ini", "java", "js",
        "json", "jsx", "log", "md", "mjs", "py", "rs", "sh", "sql", "toml", "ts", "tsx", "txt",
        "xml", "yaml", "yml",
    ];
    let name = basename(path);
    name.rsplit_once('.')
        .is_some_and(|(_, ext)| EXTENSIONS.contains(&ext.to_lowercase().as_str()))
}

/// Node `inferAttachmentMimeFromPath` (with `inferVideoMimeFromPath`).
pub fn infer_mime(path: &str) -> &'static str {
    let lower = path.to_lowercase();
    let ext = lower.rsplit_once('.').map_or("", |(_, ext)| ext);
    match ext {
        "pdf" => "application/pdf",
        "json" => "application/json",
        "csv" => "text/csv",
        "md" => "text/markdown",
        "mp4" => "video/mp4",
        "m4v" => "video/x-m4v",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        _ if text_like(path) => "text/plain",
        _ => "application/octet-stream",
    }
}
