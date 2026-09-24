//! Read for images and videos (Node `read-image.ts`, `read-video.ts` and the
//! Jimp image processor): the media become a model content block saved in the
//! session's tool results. Spec rust-m5-tools §5.
use crate::contract::ToolOutput;
use crate::domain::session::StoredAttachment;
use anyhow::{Result, bail};
use image::{DynamicImage, ImageFormat, imageops::FilterType};
use serde_json::{Value, json};
use std::path::Path;
use tokio_util::sync::CancellationToken;

const IMAGE_MAX_INPUT: u64 = 20 * 1024 * 1024;
const VIDEO_MAX_INPUT: u64 = 30 * 1024 * 1024;
const MAX_DIMENSION: u32 = 2000;
/// Node `READ_IMAGE_MAX_BASE64_BYTES`, `READ_IMAGE_TARGET_BYTES`, `READ_MAX_OUTPUT_TOKENS`.
const MAX_BASE64: usize = 5 * 1024 * 1024;
const MAX_RAW: usize = MAX_BASE64 * 3 / 4;
const MAX_TOKENS: usize = 25_000;
const JPEG_QUALITIES: [u8; 4] = [80, 60, 40, 20];
const SCALES: [f64; 3] = [0.75, 0.5, 0.25];
const AGGRESSIVE_EDGES: [u32; 6] = [1000, 800, 600, 400, 300, 200];

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

/// Node `fitsImageBudget`.
fn fits(len: usize) -> bool {
    let base64 = len.div_ceil(3) * 4;
    len <= MAX_RAW && base64 <= MAX_BASE64 && base64.div_ceil(8) <= MAX_TOKENS
}

/// Node `detectImageMediaType`: the file's signature over its extension.
fn sniff(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

struct Candidate {
    data: Vec<u8>,
    mime: &'static str,
    width: u32,
    height: u32,
}

fn encode(image: &DynamicImage, mime: &'static str, quality: u8) -> Option<Candidate> {
    let mut data = std::io::Cursor::new(vec![]);
    let written = match mime {
        "image/jpeg" => {
            let rgb = DynamicImage::ImageRgb8(image.to_rgb8());
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut data, quality);
            rgb.write_with_encoder(encoder)
        }
        "image/gif" => image.write_to(&mut data, ImageFormat::Gif),
        _ => {
            let encoder = image::codecs::png::PngEncoder::new_with_quality(
                &mut data,
                image::codecs::png::CompressionType::Best,
                image::codecs::png::FilterType::Adaptive,
            );
            image.write_with_encoder(encoder)
        }
    };
    written.ok()?;
    Some(Candidate {
        data: data.into_inner(),
        mime,
        width: image.width(),
        height: image.height(),
    })
}

fn fitting(candidate: Option<Candidate>) -> Option<Candidate> {
    candidate.filter(|c| fits(c.data.len()))
}

fn jpeg_steps(image: &DynamicImage) -> Option<Candidate> {
    JPEG_QUALITIES
        .iter()
        .find_map(|q| fitting(encode(image, "image/jpeg", *q)))
}

fn preserving(image: &DynamicImage, mime: &'static str) -> Option<Candidate> {
    match mime {
        "image/png" => fitting(encode(image, "image/png", 0)),
        "image/jpeg" => jpeg_steps(image),
        "image/gif" => fitting(encode(image, "image/gif", 0)),
        _ => None,
    }
}

fn to_max_edge(image: &DynamicImage, edge: u32) -> DynamicImage {
    if image.width().max(image.height()) <= edge {
        return image.clone();
    }
    image.resize(edge, edge, FilterType::CatmullRom)
}

/// Node `findFirstFittingCandidate`, in its order.
fn search(image: &DynamicImage, source: &'static str) -> Option<Candidate> {
    let within = image.width() <= MAX_DIMENSION && image.height() <= MAX_DIMENSION;
    // PNG 只做一次原尺寸无损尝试，之后单向转 JPEG（Node 同样的顺序）。
    let keep_format = source != "image/png";
    if within && let Some(c) = preserving(image, source) {
        return Some(c);
    }
    let bounded = to_max_edge(image, MAX_DIMENSION);
    let resized = (bounded.width(), bounded.height()) != (image.width(), image.height());
    if keep_format
        && resized
        && let Some(c) = fitting(encode(&bounded, source, 100))
    {
        return Some(c);
    }
    if !within
        && keep_format
        && let Some(c) = preserving(&bounded, source)
    {
        return Some(c);
    }
    if let Some(c) = jpeg_steps(&bounded) {
        return Some(c);
    }
    let longest = f64::from(bounded.width().max(bounded.height()));
    for scale in SCALES {
        let scaled = to_max_edge(&bounded, ((longest * scale).round() as u32).max(1));
        if keep_format && let Some(c) = preserving(&scaled, source) {
            return Some(c);
        }
        if let Some(c) = jpeg_steps(&scaled) {
            return Some(c);
        }
    }
    AGGRESSIVE_EDGES.iter().find_map(|edge| {
        let scaled = to_max_edge(image, (*edge).min(MAX_DIMENSION));
        fitting(encode(&scaled, "image/jpeg", 20))
    })
}

/// A prepared image: bytes, MIME and Node's structured fields.
pub(super) struct Prepared {
    pub data: Vec<u8>,
    pub mime: &'static str,
    pub info: Value,
}

/// Node `prepareJimpImageForModel`.
pub(super) fn prepare(data: Vec<u8>, extension_mime: &'static str) -> Result<Prepared, String> {
    if data.is_empty() {
        return Err("Image file is empty (0 bytes)".into());
    }
    let source = sniff(&data).unwrap_or(extension_mime);
    let size = data.len();
    let original = |data: Vec<u8>, dims: Option<(u32, u32)>| {
        let dims = dims.map_or(json!({}), |(w, h)| {
            json!({"originalWidth": w, "originalHeight": h, "displayWidth": w, "displayHeight": h})
        });
        let info = json!({"originalSize": size, "transformedSize": size, "resized": false,
            "compressed": false, "compressionStrategy": "original", "dimensions": dims});
        Prepared {
            data,
            mime: source,
            info,
        }
    };
    if source == "image/webp" {
        if !fits(size) {
            return Err("WebP image exceeds the model image budget and the current image adapter cannot transcode WebP".into());
        }
        return Ok(original(data, None));
    }
    let image =
        image::load_from_memory(&data).map_err(|_| "Unable to decode image data".to_owned())?;
    let (width, height) = (image.width(), image.height());
    if width <= MAX_DIMENSION && height <= MAX_DIMENSION && fits(size) {
        return Ok(original(data, Some((width, height))));
    }
    let Some(candidate) = search(&image, source) else {
        return Err(format!(
            "Unable to compress image ({size} bytes) within the requested model image budget"
        ));
    };
    let resized = (candidate.width, candidate.height) != (width, height);
    let info = json!({"originalSize": size, "transformedSize": candidate.data.len(),
        "resized": resized,
        "compressed": candidate.data.len() < size || candidate.mime != source,
        "dimensions": {"originalWidth": width, "originalHeight": height,
            "displayWidth": candidate.width, "displayHeight": candidate.height}});
    Ok(Prepared {
        data: candidate.data,
        mime: candidate.mime,
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
    let mut output = ToolOutput::new(format!("[Attached {mime}: {placeholder}]"), info);
    output.model_content = Some(json!([{"type": "_zcode_attachment", "asset": asset,
        "name": name, "placeholder": placeholder}]));
    Ok(output)
}

#[cfg(test)]
#[path = "read_media_tests.rs"]
mod tests;
