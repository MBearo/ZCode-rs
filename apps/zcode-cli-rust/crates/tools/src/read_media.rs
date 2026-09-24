//! Read for images and videos (Node `read-image.ts`, `read-video.ts` and the
//! Jimp image processor): the media become a model content block saved in the
//! session's tool results. Spec rust-m5-tools §5.
use crate::contract::ToolOutput;
use crate::domain::session::StoredAttachment;
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::path::Path;
use tokio_util::sync::CancellationToken;
use zcode_cli_host::image::{self as processor, Budget};

const IMAGE_MAX_INPUT: u64 = 20 * 1024 * 1024;
const VIDEO_MAX_INPUT: u64 = 30 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Media {
    Image(&'static str),
    Video(&'static str),
}

/// Node `inferImageMimeFromPath` / `inferVideoMimeFromPath`.
pub(super) fn kind(path: &Path) -> Option<Media> {
    let lower = path.to_string_lossy().to_lowercase();
    let image = [
        (".jpg", "image/jpeg"),
        (".jpeg", "image/jpeg"),
        (".png", "image/png"),
        (".gif", "image/gif"),
        (".webp", "image/webp"),
    ];
    let video = [
        (".mp4", "video/mp4"),
        (".m4v", "video/x-m4v"),
        (".mov", "video/quicktime"),
        (".webm", "video/webm"),
        (".mkv", "video/x-matroska"),
        (".avi", "video/x-msvideo"),
    ];
    if let Some((_, mime)) = image.iter().find(|(ext, _)| lower.ends_with(ext)) {
        return Some(Media::Image(mime));
    }
    video
        .iter()
        .find(|(ext, _)| lower.ends_with(ext))
        .map(|(_, mime)| Media::Video(mime))
}

/// Node adapter `formatByteCount`.
fn byte_count(bytes: u64) -> String {
    let unit = |value: f64| {
        if value.fract() == 0.0 {
            format!("{value}")
        } else {
            let text = format!("{value:.1}");
            text.strip_suffix(".0").map(str::to_owned).unwrap_or(text)
        }
    };
    match bytes {
        b if b < 1024 => format!("{b}B"),
        b if b < 1024 * 1024 => format!("{}KB", unit(b as f64 / 1024.0)),
        b => format!("{}MB", unit(b as f64 / (1024.0 * 1024.0))),
    }
}

/// A prepared image: bytes, MIME and Node's Read result fields.
pub(super) struct Prepared {
    pub data: Vec<u8>,
    pub mime: &'static str,
    pub info: Value,
}

/// Node `read-image.ts` over the shared image processor (Read budget).
pub(super) fn prepare(data: Vec<u8>, extension_mime: &'static str) -> Result<Prepared, String> {
    let p = processor::prepare(data, extension_mime, Budget::READ)?;
    let size = p.original_size;
    let dims = p.dimensions.map_or(json!({}), |d| {
        json!({"originalWidth": d.original_width, "originalHeight": d.original_height,
            "displayWidth": d.width, "displayHeight": d.height})
    });
    let info = if p.original {
        json!({"originalSize": size, "transformedSize": size, "resized": false,
            "compressed": false, "compressionStrategy": "original", "dimensions": dims})
    } else {
        json!({"originalSize": size, "transformedSize": p.data.len(), "resized": p.resized,
            "compressed": p.compressed, "dimensions": dims})
    };
    Ok(Prepared {
        data: p.data,
        mime: p.mime,
        info,
    })
}

async fn load(path: &Path, max: u64) -> Result<Vec<u8>> {
    let size = tokio::fs::metadata(path).await?.len();
    if size > max {
        bail!(
            "File content ({}) exceeds maximum allowed size ({}). Use a smaller file.",
            byte_count(size),
            byte_count(max)
        );
    }
    Ok(tokio::fs::read(path).await?)
}

/// Saves `data` in the session's tool results for the model request.
async fn save(
    data: &[u8],
    mime: &str,
    source: &Path,
    artifacts: &Path,
) -> Result<StoredAttachment> {
    let extension = mime.rsplit('/').next().unwrap_or("bin");
    let directory = artifacts.join("read-media");
    tokio::fs::create_dir_all(&directory).await?;
    let path = directory.join(format!("{}.{extension}", uuid::Uuid::new_v4()));
    tokio::fs::write(&path, data).await?;
    Ok(StoredAttachment {
        path: path.to_string_lossy().into_owned(),
        source_path: Some(source.to_string_lossy().into_owned()),
        media_type: mime.into(),
        total_bytes: data.len() as u64,
        ..Default::default()
    })
}

/// Node Read of an image or video file.
pub(super) async fn read(
    path: &Path,
    media: Media,
    artifacts: &Path,
    cancel: &CancellationToken,
) -> Result<ToolOutput> {
    let (mime, data, placeholder, mut info) = match media {
        Media::Image(mime) => {
            let bytes = load(path, IMAGE_MAX_INPUT).await?;
            let work = tokio::task::spawn_blocking(move || prepare(bytes, mime));
            let prepared = tokio::select! {
                _ = cancel.cancelled() => bail!("Cancelled"),
                result = work => result?.map_err(anyhow::Error::msg)?,
            };
            (prepared.mime, prepared.data, "Read image", prepared.info)
        }
        Media::Video(mime) => {
            let bytes = load(path, VIDEO_MAX_INPUT).await?;
            if bytes.is_empty() {
                bail!("Cannot read an empty video file.");
            }
            let info = json!({"originalSize": bytes.len()});
            (mime, bytes, "Read video", info)
        }
    };
    let asset = save(&data, mime, path, artifacts).await?;
    let kind = if matches!(media, Media::Image(_)) {
        "image"
    } else {
        "video"
    };
    info["type"] = kind.into();
    info["mimeType"] = mime.into();
    let name = path
        .file_name()
        .map_or(String::new(), |n| n.to_string_lossy().into_owned());
    // Node source.sizeBytes：原始文件大小（Node 落库的 file part 用到）。
    let size = info["originalSize"].clone();
    let mut output = ToolOutput::new(format!("[Attached {mime}: {placeholder}]"), info);
    output.model_content = Some(json!([{"type": "_zcode_attachment", "asset": asset,
        "name": name, "placeholder": placeholder, "sizeBytes": size}]));
    Ok(output)
}

#[cfg(test)]
#[path = "read_media_tests.rs"]
mod tests;
