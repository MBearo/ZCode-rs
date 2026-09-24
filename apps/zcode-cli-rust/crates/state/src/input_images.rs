//! Node `prepareImageDataUrl` of prompt images (spec rust-m11-node-storage
//! §5.3): the shared image processor off the async runtime, and uploaded
//! images whose artifact keeps the original while requests send the
//! prepared bytes.
use super::input_attachments::{Backing, read_attachment};
use crate::domain::node_journal::files::{self, Media, NodeFile};
use crate::domain::session::StoredAttachment;
use anyhow::Result;
use zcode_cli_host::image::{self as processor, Budget, Prepared};

/// Prepares image `bytes` (`mime` when the bytes carry no known signature)
/// in a blocking task; the inner error is the processor's.
pub(crate) async fn prepare(
    bytes: Vec<u8>,
    mime: &'static str,
) -> Result<Result<Prepared, String>> {
    Ok(
        tokio::task::spawn_blocking(move || processor::prepare(bytes, mime, Budget::ATTACHMENT))
            .await?,
    )
}

/// The processor's fallback type of a submitted image MIME (it sniffs the
/// bytes first; formats it cannot decode fail either way).
fn submitted(mime: &str) -> &'static str {
    match mime.to_ascii_lowercase().as_str() {
        "image/jpeg" | "image/jpg" => "image/jpeg",
        "image/gif" => "image/gif",
        "image/webp" => "image/webp",
        _ => "image/png",
    }
}

impl Backing {
    /// Node `resolveInlineMediaAttachment` of the uploaded image `asset`
    /// (`reference` is its artifact): the resolved file, and the prepared
    /// asset this run's requests send when it differs from the upload.
    pub(crate) async fn uploaded_image(
        &self,
        reference: &str,
        asset: &StoredAttachment,
        file_name: &str,
        index: usize,
    ) -> Result<(NodeFile, Option<StoredAttachment>)> {
        let bytes = read_attachment(asset, 0, asset.total_bytes as usize).await?;
        let failed = |code| async move {
            let data_url = tokio::fs::read_to_string(&asset.path).await?;
            anyhow::Ok((files::upload_image_failed(&data_url, index, code), None))
        };
        // Node：data URL 没有正文时是无效图片，处理器报错时是缩放失败。
        if bytes.is_empty() {
            return failed("attachment_image_invalid").await;
        }
        let prepared = match prepare(bytes, submitted(&asset.media_type)).await? {
            Ok(prepared) => prepared,
            Err(_) => return failed("attachment_image_resize_failed").await,
        };
        let file = files::media(Media {
            uri: reference,
            mime: prepared.mime,
            bytes: asset.total_bytes,
            file_name,
            index,
            local: None,
            image: Some((&asset.media_type, prepared.attachment_metadata())),
        });
        // Node 的请求总是用准备后的 data URL（类型取嗅探结果）；字节与类型都未变时直接用上传产物。
        if prepared.original && prepared.mime.eq_ignore_ascii_case(&asset.media_type) {
            return Ok((file, None));
        }
        let extension = prepared.mime.rsplit('/').next().unwrap_or("bin");
        let directory = self.cache.join("prompt-images");
        tokio::fs::create_dir_all(&directory).await?;
        let path = directory.join(format!("{}.{extension}", uuid::Uuid::new_v4()));
        tokio::fs::write(&path, &prepared.data).await?;
        let live = StoredAttachment {
            path: path.to_string_lossy().into_owned(),
            media_type: prepared.mime.into(),
            total_bytes: prepared.data.len() as u64,
            ..Default::default()
        };
        Ok((file, Some(live)))
    }
}
