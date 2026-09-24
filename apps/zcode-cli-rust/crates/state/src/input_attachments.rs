//! Prompt attachment bytes (spec rust-m11-node-storage §5.3): Node data URL
//! artifacts for the Node store, content-addressed blobs for the legacy
//! store, and the Node resolution of local path attachments.
use crate::domain::node_journal::files::{self, Kind, Media, TextRead};
use crate::domain::session::StoredAttachment;
use anyhow::{Result, ensure};
use base64::Engine as _;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

/// Node's inline media budget (`INLINE_MEDIA_ATTACHMENT_MAX_BYTES`, the PDF
/// limit; Rust applies it to video as well).
const MEDIA_MAX_BYTES: u64 = 20 * 1024 * 1024;

/// Where attachment bytes live: Node's artifact root (`data:` URL text artifacts).
#[derive(Clone)]
pub(crate) enum Backing {
    Artifacts(PathBuf),
}

impl Backing {
    /// Stores `chunks` for `session` under an artifact named after `call`.
    pub(crate) async fn put(
        &self,
        session: &str,
        call: &str,
        chunks: &[Vec<u8>],
        mime: &str,
    ) -> Result<(String, StoredAttachment)> {
        match self {
            Self::Artifacts(root) => {
                super::node::artifacts::write_data_url(root, session, call, chunks, mime).await
            }
        }
    }

    /// The stored bytes behind `reference` (Node artifacts only).
    pub(crate) async fn attachment_of(&self, reference: &str) -> Result<Option<StoredAttachment>> {
        match self {
            Self::Artifacts(root) => super::node::artifacts::stored(root, reference).await,
        }
    }

    /// Node `resolveLocalMediaAttachment` / `resolveLocalFileAttachment` of the
    /// attachment at `index`: `original` as submitted, `path` resolved (a
    /// `file://` URL or an absolute path). Media become artifacts; text files
    /// are previewed; other files and read failures stay path references.
    pub(crate) async fn local(
        &self,
        session: &str,
        index: usize,
        (original, path): (&str, &str),
        mime: &str,
    ) -> Result<(String, StoredAttachment)> {
        let path = file_path(path)?;
        let shown = path.to_string_lossy().into_owned();
        let local = (original, shown.as_str());
        let kind = files::kind(mime);
        let plain = |node, media_type: &str, size| {
            let stored = StoredAttachment {
                path: shown.clone(),
                source_path: Some(shown.clone()),
                media_type: media_type.into(),
                total_bytes: size,
                node: Some(node),
                ..Default::default()
            };
            Ok((original.to_owned(), stored))
        };
        let failed_mime = if kind == Kind::Other {
            "text/plain"
        } else {
            mime
        };
        let failed = |code| {
            plain(
                files::local_failed(local, failed_mime, code),
                failed_mime,
                0,
            )
        };
        let Ok(meta) = tokio::fs::metadata(&path).await else {
            return failed("attachment_read_failed");
        };
        if !meta.is_file() {
            return failed("attachment_not_file");
        }
        let size = meta.len();
        if kind == Kind::Other {
            if !files::text_like(&shown) {
                let mime = files::infer_mime(&shown);
                return plain(
                    files::local_reference(local, mime, size, "binary_file"),
                    mime,
                    size,
                );
            }
            let Ok(bytes) = tokio::fs::read(&path).await else {
                return failed("attachment_read_failed");
            };
            let text = String::from_utf8_lossy(&bytes).replace("\r\n", "\n");
            let body = text.strip_suffix('\n').unwrap_or(&text);
            let lines: Vec<&str> = body.split('\n').collect();
            let total_lines = if text.is_empty() {
                0
            } else {
                lines.len() as u64
            };
            // Node：超过 256 KiB 的文本附件只取前 2000 行，标记为截断预览。
            let truncated = size > files::READ_MAX_BYTES;
            let content = if truncated {
                lines[..lines.len().min(files::READ_MAX_LINES)].join("\n")
            } else {
                body.to_owned()
            };
            let read = TextRead {
                content: &content,
                truncated,
                size,
                total_lines,
            };
            return plain(files::local_text(local, read), "text/plain", size);
        }
        if size > MEDIA_MAX_BYTES {
            let reason = match kind {
                Kind::Image => "image_too_large",
                Kind::Pdf => "pdf_too_large",
                _ => "video_too_large",
            };
            return plain(
                files::local_reference(local, mime, size, reason),
                mime,
                size,
            );
        }
        let Ok(bytes) = read_stable(&path, size).await else {
            return failed("attachment_read_failed");
        };
        match kind {
            Kind::Pdf if !bytes.starts_with(b"%PDF-") => return failed("attachment_pdf_invalid"),
            Kind::Video if bytes.is_empty() => return failed("attachment_video_invalid"),
            Kind::Image if bytes.is_empty() => return failed("attachment_image_invalid"),
            _ => {}
        }
        let sha = format!("sha256:{:x}", Sha256::digest(&bytes));
        let call = format!("attachment-{}", index + 1);
        let (uri, mut stored) = self.put(session, &call, &[bytes], mime).await?;
        stored.source_path = Some(shown.clone());
        stored.node = Some(files::media(Media {
            uri: &uri,
            mime,
            bytes: stored.total_bytes,
            file_name: original,
            index,
            local: Some((local, &sha)),
        }));
        Ok((uri, stored))
    }
}

fn file_path(path: &str) -> Result<PathBuf> {
    if path.starts_with("file://") {
        url::Url::parse(path)?
            .to_file_path()
            .map_err(|_| anyhow::anyhow!("Invalid attachment file URL"))
    } else {
        Ok(path.into())
    }
}

/// The bytes of a file that stays unchanged while it is read.
async fn read_stable(path: &Path, size: u64) -> Result<Vec<u8>> {
    let mut file = tokio::fs::File::open(path).await?;
    let before = file.metadata().await?;
    let mut bytes = Vec::with_capacity(size as usize);
    (&mut file)
        .take(MEDIA_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .await?;
    let after = file.metadata().await?;
    // 同一次 admission 不能把变更中的文件伪装成稳定快照；排队提升只读取已提交副本。
    ensure!(
        bytes.len() as u64 == before.len()
            && after.len() == before.len()
            && after.modified()? == before.modified()?,
        "Attachment changed during snapshot"
    );
    Ok(bytes)
}

/// Reads `limit` bytes at `offset` of a stored attachment that is unchanged.
pub(super) async fn read_attachment(
    asset: &StoredAttachment,
    offset: u64,
    limit: usize,
) -> Result<Vec<u8>> {
    if asset.data_url {
        return read_data_url(asset, offset, limit).await;
    }
    let mut file = tokio::fs::File::open(&asset.path).await?;
    ensure!(
        file.metadata().await?.len() == asset.total_bytes,
        "Attachment snapshot changed"
    );
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    let mut bytes = vec![];
    file.take(limit as u64).read_to_end(&mut bytes).await?;
    Ok(bytes)
}

/// Decodes only the base64 groups covering `offset..offset + limit` of a
/// `data:` URL artifact.
async fn read_data_url(asset: &StoredAttachment, offset: u64, limit: usize) -> Result<Vec<u8>> {
    let path = Path::new(&asset.path);
    let (_, header) = super::node::artifacts::data_url_header(path)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Attachment artifact is not a data URL"))?;
    let mut file = tokio::fs::File::open(path).await?;
    let total = asset.total_bytes;
    ensure!(
        file.metadata().await?.len() - header == total.div_ceil(3) * 4,
        "Attachment snapshot changed"
    );
    let start = offset.min(total);
    let end = start.saturating_add(limit as u64).min(total);
    if start == end {
        return Ok(vec![]);
    }
    let (first, last) = (start / 3, end.div_ceil(3));
    file.seek(std::io::SeekFrom::Start(header + first * 4))
        .await?;
    let mut encoded = vec![0; ((last - first) * 4) as usize];
    file.read_exact(&mut encoded).await?;
    let decoded = base64::engine::general_purpose::STANDARD.decode(&encoded)?;
    let skip = (start - first * 3) as usize;
    Ok(decoded[skip..skip + (end - start) as usize].to_vec())
}

#[cfg(test)]
#[path = "input_attachments_tests.rs"]
mod tests;
