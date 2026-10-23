//! Artifact 文件：`<storage.dir>/cli/artifacts/<会话目录>/`，URI 为
//! `zcode-artifact://<sessionId>/<artifactId>`（docs/specs/rust-m11-node-storage.md §2.1）。
//!
//! 命名沿用 Node `NodeToolArtifactStore`（复制来源 `apps/zcode-cli-rust/crates/state/src/node/artifacts.rs`）：
//! 会话目录是 `sanitizePathSegment(sessionId)`，artifact id 是 `tool-result-<uuid>`，文件名
//! `<前缀>-<artifactId><扩展名>` 含 artifact id，Node 按 id 子串找到文件；扩展名决定 Node 按文本
//! 还是 base64 读回。前缀是用途（Node 用 toolCallId，内核的 put 没有它）。
use rust_cli_kernel_api::ids::{ArtifactId, SessionId};
use rust_cli_kernel_api::persisted_output::ARTIFACT_SCHEME;
use rust_cli_kernel_api::store::{ArtifactPurpose, ArtifactRef, StoreError};
use rust_cli_paths::StoragePath;

use crate::storage;

/// Node `sanitizePathSegment`（JS 正则按 UTF-16 码元替换）。
pub fn segment(value: &str) -> String {
    let mut out = String::new();
    for c in value.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
        } else {
            out.extend(std::iter::repeat_n('_', c.len_utf16()));
        }
    }
    out.truncate(120);
    if out.is_empty() {
        "unknown".into()
    } else {
        out
    }
}

/// JS `encodeURIComponent`。
fn encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Artifact 的 URI（写进 part 时使用）。
pub fn uri(artifact: &ArtifactRef) -> String {
    format!(
        "{ARTIFACT_SCHEME}{}/{}",
        encode(artifact.session.as_str()),
        encode(artifact.id.as_str())
    )
}

/// 扩展名：文本类型用 Node 按 UTF-8 读回的扩展名（`.txt`、`.md`、`.json`、`.html`、`.csv`），图片与
/// PDF 用各自的扩展名，其余是 `.bin`（Node 按 base64 读回，不会误当成 JSON）。
fn extension(media_type: &str) -> &'static str {
    let mime = media_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match mime.as_str() {
        "text/plain" => ".txt",
        "text/markdown" => ".md",
        "text/html" => ".html",
        "text/csv" => ".csv",
        "application/json" => ".json",
        "image/png" => ".png",
        "image/jpeg" | "image/jpg" => ".jpg",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        "application/pdf" => ".pdf",
        _ => ".bin",
    }
}

/// 扩展名的反解（Node `contentTypeForFileName` 里本工程会遇到的几种；其余按二进制）。
pub(crate) fn media_type(name: &str) -> &'static str {
    let extension = name.rsplit_once('.').map_or("", |(_, ext)| ext);
    match extension.to_ascii_lowercase().as_str() {
        "json" => "application/json",
        "txt" => "text/plain",
        "md" => "text/markdown",
        "html" => "text/html",
        "csv" => "text/csv",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

const PURPOSES: [ArtifactPurpose; 5] = [
    ArtifactPurpose::ToolOutput,
    ArtifactPurpose::Attachment,
    ArtifactPurpose::Media,
    ArtifactPurpose::Checkpoint,
    ArtifactPurpose::Deliverable,
];

fn prefix(purpose: ArtifactPurpose) -> &'static str {
    match purpose {
        ArtifactPurpose::ToolOutput => "tool_output",
        ArtifactPurpose::Attachment => "attachment",
        ArtifactPurpose::Media => "media",
        ArtifactPurpose::Checkpoint => "checkpoint",
        ArtifactPurpose::Deliverable => "deliverable",
    }
}

/// Node 的 artifact id 格式。
pub fn artifact_id(uuid: &str) -> String {
    format!("tool-result-{uuid}")
}

/// Artifact 文件的位置；由引用完全决定，读取不用扫描目录。
pub fn path(root: &StoragePath, artifact: &ArtifactRef) -> StoragePath {
    root.join(segment(artifact.session.as_str())).join(format!(
        "{}-{}{}",
        prefix(artifact.purpose),
        segment(artifact.id.as_str()),
        extension(&artifact.media_type)
    ))
}

/// 按会话与 artifact id 找回引用（`ArtifactStore::find`，修复线 F18，docs/specs/rust-cli/node-store.md §4.4）。
///
/// 在会话的产物目录里找名字以 `-<artifact><扩展名>` 结尾的文件：本工程的 `<用途>-<artifact>`，或 Node 的
/// `<调用 id>-<artifact>`。用途取名字的前缀（不是本工程的用途名时按 `ToolOutput`，`get` 按 Node 的读法找到它），
/// 类型按扩展名，大小是文件长度。按结尾精确匹配、不按子串：不完整的 id 找不到。位置来自模型：会话目录名是
/// `.` / `..`、id 为空时当作找不到，不走出产物目录。
pub async fn find(
    root: &StoragePath,
    session: &SessionId,
    id: &ArtifactId,
) -> Result<Option<ArtifactRef>, StoreError> {
    let directory = segment(session.as_str());
    if id.as_str().is_empty() || directory == "." || directory == ".." {
        return Ok(None);
    }
    let dir = root.join(&directory);
    let names = rust_cli_fs::file_names(dir.as_path())
        .await
        .map_err(storage)?;
    let tail = format!("-{}", segment(id.as_str()));
    let Some((name, owner)) = names.iter().find_map(|name| {
        let (stem, _) = name.rsplit_once('.')?;
        stem.strip_suffix(tail.as_str()).map(|owner| (name, owner))
    }) else {
        return Ok(None);
    };
    let Some(size) = rust_cli_fs::file_len(dir.join(name).as_path())
        .await
        .map_err(storage)?
    else {
        return Ok(None);
    };
    let purpose = PURPOSES
        .into_iter()
        .find(|purpose| prefix(*purpose) == owner)
        .unwrap_or(ArtifactPurpose::ToolOutput);
    Ok(Some(ArtifactRef {
        session: session.clone(),
        id: id.clone(),
        purpose,
        media_type: media_type(name).to_owned(),
        size,
    }))
}

#[cfg(test)]
#[path = "artifacts_tests.rs"]
mod tests;
