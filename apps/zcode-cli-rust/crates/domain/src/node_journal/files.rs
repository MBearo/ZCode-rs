//! Node prompt attachments (`mapAttachmentRefsToTurnAttachments`,
//! `resolveTurnAttachments`): the stored `file` part and model block of each
//! attachment, and their layout in the model input
//! (`buildRuntimeUserEntriesFromTurn`). Spec rust-m11-node-storage §5.3.
//! Pure: the engine reads the bytes and writes the artifacts.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// Node `INLINE_TEXT_ATTACHMENT_MAX_BYTES` of a non-media upload.
pub const INLINE_TEXT_MAX_BYTES: u64 = 64 * 1024;
/// Node `READ_MAX_FILE_SIZE_BYTES` of a local text attachment.
pub const READ_MAX_BYTES: u64 = 256 * 1024;
/// Node `READ_DEFAULT_MAX_LINES` of an oversized local text attachment.
pub const READ_MAX_LINES: usize = 2_000;

/// One resolved attachment.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct NodeFile {
    /// The `file` part without `id`, `sessionID` and `messageID`.
    pub part: Value,
    /// Node `contentBlock`; media blocks omit `dataUrl` (the bytes stay in
    /// the artifact).
    pub block: Value,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Image,
    Video,
    Pdf,
    Other,
}

/// The media kind of `mime` (parameters and case ignored).
pub fn kind(mime: &str) -> Kind {
    let mime = mime.split(';').next().unwrap_or("").trim().to_lowercase();
    if mime.starts_with("image/") {
        Kind::Image
    } else if mime.starts_with("video/") {
        Kind::Video
    } else if mime == "application/pdf" {
        Kind::Pdf
    } else {
        Kind::Other
    }
}

/// The length of the `data:<mime>;base64,` URL of `bytes` bytes.
pub fn data_url_len(mime: &str, bytes: u64) -> u64 {
    ("data:".len() + mime.len() + ";base64,".len()) as u64 + bytes.div_ceil(3) * 4
}

/// Node `path.basename` (POSIX separators).
fn basename(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
}

/// Node `persistUserPrompt` file part order: `type, mime, filename, url,
/// source, metadata` (undefined members omitted).
fn part(
    mime: &str,
    filename: Option<&str>,
    url: &str,
    source: Option<Value>,
    meta: Value,
) -> Value {
    let mut out = Map::new();
    out.insert("type".into(), "file".into());
    out.insert("mime".into(), mime.into());
    if let Some(filename) = filename {
        out.insert("filename".into(), filename.into());
    }
    out.insert("url".into(), url.into());
    if let Some(source) = source {
        out.insert("source".into(), source);
    }
    out.insert("metadata".into(), meta);
    Value::Object(out)
}

/// Node `FilePartSource` of a local path attachment.
fn local_source(original: &str, path: &str) -> Value {
    json!({"type": "file", "path": path,
        "text": {"value": original, "start": 0, "end": original.encode_utf16().count()}})
}

fn text_block(text: &str) -> Value {
    json!({"type": "text", "text": text})
}

/// A local file behind an attachment: `(original ref, absolute path)`.
pub type Local<'a> = (&'a str, &'a str);

/// An image, video or PDF stored as a data URL artifact.
pub struct Media<'a> {
    pub uri: &'a str,
    pub mime: &'a str,
    /// Decoded bytes.
    pub bytes: u64,
    /// The attachment's `fileName`.
    pub file_name: &'a str,
    /// 0-based position among the input's attachments.
    pub index: usize,
    /// The local file and its `sha256:<hex>` for a path attachment.
    pub local: Option<(Local<'a>, &'a str)>,
}

/// Node `resolveInlineMediaAttachment` (uploads) and
/// `resolveLocalMediaAttachment` (path attachments).
pub fn media(m: Media) -> NodeFile {
    let kind = kind(m.mime);
    let mut meta = Map::new();
    if let Some(((original, _), _)) = m.local {
        meta.insert("originalUrl".into(), original.into());
    }
    meta.insert("recoverability".into(), "provider_ready".into());
    if let Some((_, sha)) = m.local {
        meta.insert("sha256".into(), sha.into());
    }
    // Node 对上传图片记录 data URL 的长度，视频与 PDF 记录解码后的字节数。
    let size = if kind == Kind::Image && m.local.is_none() {
        data_url_len(m.mime, m.bytes)
    } else {
        m.bytes
    };
    meta.insert("sizeBytes".into(), size.into());
    meta.insert("storageKind".into(), "artifact".into());
    meta.insert("artifactUri".into(), m.uri.into());
    let (filename, source, placeholder) = match m.local {
        Some(((original, path), _)) => (
            Some(basename(path)),
            Some(local_source(original, path)),
            original,
        ),
        None => (
            (kind == Kind::Pdf).then_some(m.file_name),
            None,
            m.file_name,
        ),
    };
    let mut block_source = json!({"id": format!("turn-attachment-{}", m.index + 1),
        "kind": if m.local.is_some() { "local_file" } else { "inline" }, "mimeType": m.mime});
    if let Some(((_, path), _)) = m.local {
        block_source["path"] = path.into();
    }
    block_source["placeholder"] = placeholder.into();
    block_source["uri"] = m.uri.into();
    let block = match kind {
        Kind::Image => json!({"type": "image", "mediaType": m.mime, "source": block_source}),
        Kind::Video => json!({"type": "video", "mediaType": m.mime, "source": block_source}),
        _ => json!({"type": "file", "mediaType": m.mime, "name": filename.unwrap_or(m.file_name),
            "source": block_source}),
    };
    NodeFile {
        part: part(m.mime, filename, m.uri, source, Value::Object(meta)),
        block,
    }
}

/// Node `resolvedInlineTextAttachment` of a decoded non-media upload.
pub fn inline_text(file_name: &str, text: &str) -> NodeFile {
    let bytes = text.len();
    let meta = json!({"originalUrl": file_name,
        "preview": {"text": text, "truncated": false, "originalBytes": bytes},
        "recoverability": "provider_ready", "sizeBytes": bytes, "storageKind": "inline"});
    NodeFile {
        part: part(
            "text/plain",
            Some(basename(file_name)),
            file_name,
            None,
            meta,
        ),
        block: text_block(text),
    }
}

/// Node `resolvedPlaceholderAttachment` of an upload whose text is not
/// inlined (over 64 KiB, empty or unreadable).
pub fn unread_upload(index: usize) -> NodeFile {
    let meta = json!({"errorCode": "attachment_read_failed", "recoverability": "metadata_only",
        "storageKind": "metadata_only"});
    NodeFile {
        part: part("text/plain", None, "", None, meta),
        block: text_block(&format!("[Attached text/plain: attachment-{}]", index + 1)),
    }
}

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

/// Node `resolvedPlaceholderAttachment` of an unreadable local file.
pub fn local_failed((original, path): Local, mime: &str, code: &str) -> NodeFile {
    let meta = json!({"errorCode": code, "originalUrl": original, "recoverability": "metadata_only",
        "storageKind": "local_ref"});
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

impl NodeFile {
    /// The stored part: Node's ids first, then the resolved members.
    pub fn record(&self, id: &str, session: &str, message: &str) -> Value {
        let mut out = Map::new();
        out.insert("id".into(), id.into());
        out.insert("sessionID".into(), session.into());
        out.insert("messageID".into(), message.into());
        if let Some(members) = self.part.as_object() {
            out.extend(members.clone());
        }
        Value::Object(out)
    }

    /// Where Node puts the attachment in the model input.
    pub fn placement(&self) -> Placement {
        let block = &self.block;
        let mime = self.part["mime"].as_str().unwrap_or("");
        match block["type"].as_str() {
            Some("image") if block["source"]["kind"] == "inline" => Placement::Pasted,
            Some("text") => {
                let text = block["text"].as_str().unwrap_or("");
                if mime.starts_with("image/") && pasted_image_placeholder(text) {
                    Placement::Pasted
                } else if let Some(body) = crate::node_history::prompt_attachment(&self.part, block)
                {
                    Placement::Reminder(body)
                } else if mime.starts_with("text/")
                    && self.part["metadata"]["errorCode"] == "attachment_read_failed"
                {
                    Placement::Skip
                } else {
                    Placement::Real
                }
            }
            _ => Placement::Real,
        }
    }
}

/// Node `/^\[Attached image\/[^:]+: \[image #\d+\]\]$/`.
fn pasted_image_placeholder(text: &str) -> bool {
    let Some(rest) = text.strip_prefix("[Attached image/") else {
        return false;
    };
    let Some((subtype, tail)) = rest.split_once(": [image #") else {
        return false;
    };
    let digits = tail.strip_suffix("]]").unwrap_or("");
    !subtype.is_empty()
        && !subtype.contains(':')
        && !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
}

/// Where an attachment goes in the model input.
#[derive(Debug, PartialEq)]
pub enum Placement {
    /// After the prompt text, in order.
    Real,
    /// After the real blocks (pasted inline images).
    Pasted,
    /// A `prompt_attachment` reminder after the user message.
    Reminder(String),
    /// Not sent (a failed text placeholder).
    Skip,
}

/// Node `buildRuntimeUserEntriesFromTurn`: the user content of `text` and
/// placed blocks, and the reminder bodies that follow it.
pub fn user_content(text: &str, files: Vec<(Placement, Value)>) -> (Value, Vec<String>) {
    let (mut real, mut pasted, mut reminders) = (vec![], vec![], vec![]);
    for (placement, block) in files {
        match placement {
            Placement::Real => real.push(block),
            Placement::Pasted => pasted.push(block),
            Placement::Reminder(body) => reminders.push(body),
            Placement::Skip => {}
        }
    }
    let mut blocks = vec![];
    if !text.is_empty() {
        blocks.push(text_block(text));
    }
    blocks.extend(real);
    blocks.extend(pasted);
    let content = match blocks.as_slice() {
        [] => Value::from(""),
        [one] if one["type"] == "text" => one["text"].clone(),
        _ => Value::Array(blocks),
    };
    (content, reminders)
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
