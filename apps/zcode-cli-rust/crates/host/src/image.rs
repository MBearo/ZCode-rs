//! Node's Jimp image processor (`adapters/src/image`, `prepareForModel`):
//! fits an image into the model image budget, first as it is, then resized
//! to the maximum edge, then as JPEG at falling quality and smaller scales.
//! Read images (spec rust-m5-tools §5) and prompt image attachments (spec
//! rust-m11-node-storage §5.3) share it.
use image::{DynamicImage, ImageFormat, imageops::FilterType};
use serde_json::{Map, Value};

/// Node `READ_IMAGE_MAX_DIMENSION` / `MAX_IMAGE_ATTACHMENT_DIMENSION`.
pub const MAX_DIMENSION: u32 = 2000;
/// Node `READ_IMAGE_MAX_BASE64_BYTES` and `READ_IMAGE_TARGET_BYTES`.
const MAX_BASE64: usize = 5 * 1024 * 1024;
const MAX_RAW: usize = MAX_BASE64 * 3 / 4;
const JPEG_QUALITIES: [u8; 4] = [80, 60, 40, 20];
const SCALES: [f64; 3] = [0.75, 0.5, 0.25];
const AGGRESSIVE_EDGES: [u32; 6] = [1000, 800, 600, 400, 300, 200];

/// Node `ImageBudget`: raw and base64 bytes, and the optional token limit.
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    max_tokens: Option<usize>,
}

impl Budget {
    /// Read: `READ_MAX_OUTPUT_TOKENS` at 8 base64 characters per token.
    pub const READ: Self = Self {
        max_tokens: Some(25_000),
    };
    /// Prompt attachments (Node `prepareImageDataUrl` passes no token limit).
    pub const ATTACHMENT: Self = Self { max_tokens: None };

    /// Node `fitsImageBudget`.
    pub fn fits(self, len: usize) -> bool {
        let base64 = len.div_ceil(3) * 4;
        len <= MAX_RAW
            && base64 <= MAX_BASE64
            && self.max_tokens.is_none_or(|max| base64.div_ceil(8) <= max)
    }
}

/// The original and shown size of a decoded image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dimensions {
    pub original_width: u32,
    pub original_height: u32,
    pub width: u32,
    pub height: u32,
}

/// Node `ImagePrepareForModelResult`.
#[derive(Debug)]
pub struct Prepared {
    pub data: Vec<u8>,
    pub mime: &'static str,
    pub original_size: usize,
    /// `None` for a WebP passed through undecoded.
    pub dimensions: Option<Dimensions>,
    pub resized: bool,
    pub compressed: bool,
    /// Node `strategy === "original"`: the input bytes unchanged.
    pub original: bool,
}

impl Prepared {
    /// Node `PreparedImageData.metadata` (`metadata.image` of a prompt
    /// attachment); members Node leaves undefined are omitted.
    pub fn attachment_metadata(&self) -> Value {
        let mut meta = Map::new();
        let dims = self.dimensions;
        if let Some(d) = dims {
            meta.insert("height".into(), d.height.into());
        }
        meta.insert("maxDimension".into(), MAX_DIMENSION.into());
        if let Some(d) = dims {
            meta.insert("originalHeight".into(), d.original_height.into());
            meta.insert("originalWidth".into(), d.original_width.into());
        }
        meta.insert("resized".into(), self.resized.into());
        meta.insert("transformedSizeBytes".into(), self.data.len().into());
        if let Some(d) = dims {
            meta.insert("width".into(), d.width.into());
        }
        Value::Object(meta)
    }
}

/// Node `detectImageMediaType`: the file's signature over its extension.
pub fn sniff(data: &[u8]) -> Option<&'static str> {
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

/// Node `inferImageMimeFromPath` (PNG when the extension is unknown).
pub fn mime_from_path(path: &str) -> &'static str {
    let lower = path.to_lowercase();
    if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else {
        "image/png"
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

fn to_max_edge(image: &DynamicImage, edge: u32) -> DynamicImage {
    if image.width().max(image.height()) <= edge {
        return image.clone();
    }
    image.resize(edge, edge, FilterType::CatmullRom)
}

impl Budget {
    fn fitting(self, candidate: Option<Candidate>) -> Option<Candidate> {
        candidate.filter(|c| self.fits(c.data.len()))
    }

    fn jpeg_steps(self, image: &DynamicImage) -> Option<Candidate> {
        JPEG_QUALITIES
            .iter()
            .find_map(|q| self.fitting(encode(image, "image/jpeg", *q)))
    }

    fn preserving(self, image: &DynamicImage, mime: &'static str) -> Option<Candidate> {
        match mime {
            "image/png" => self.fitting(encode(image, "image/png", 0)),
            "image/jpeg" => self.jpeg_steps(image),
            "image/gif" => self.fitting(encode(image, "image/gif", 0)),
            _ => None,
        }
    }

    /// Node `findFirstFittingCandidate`, in its order.
    fn search(self, image: &DynamicImage, source: &'static str) -> Option<Candidate> {
        let within = image.width() <= MAX_DIMENSION && image.height() <= MAX_DIMENSION;
        // PNG 只做一次原尺寸无损尝试，之后单向转 JPEG（Node 同样的顺序）。
        let keep_format = source != "image/png";
        if within && let Some(c) = self.preserving(image, source) {
            return Some(c);
        }
        let bounded = to_max_edge(image, MAX_DIMENSION);
        let resized = (bounded.width(), bounded.height()) != (image.width(), image.height());
        if keep_format
            && resized
            && let Some(c) = self.fitting(encode(&bounded, source, 100))
        {
            return Some(c);
        }
        if !within
            && keep_format
            && let Some(c) = self.preserving(&bounded, source)
        {
            return Some(c);
        }
        if let Some(c) = self.jpeg_steps(&bounded) {
            return Some(c);
        }
        let longest = f64::from(bounded.width().max(bounded.height()));
        for scale in SCALES {
            let scaled = to_max_edge(&bounded, ((longest * scale).round() as u32).max(1));
            if keep_format && let Some(c) = self.preserving(&scaled, source) {
                return Some(c);
            }
            if let Some(c) = self.jpeg_steps(&scaled) {
                return Some(c);
            }
        }
        AGGRESSIVE_EDGES.iter().find_map(|edge| {
            let scaled = to_max_edge(image, (*edge).min(MAX_DIMENSION));
            self.fitting(encode(&scaled, "image/jpeg", 20))
        })
    }
}

/// Node `prepareJimpImageForModel`; errors are Node's texts. CPU bound: call
/// it from a blocking task.
pub fn prepare(
    data: Vec<u8>,
    extension_mime: &'static str,
    budget: Budget,
) -> Result<Prepared, String> {
    if data.is_empty() {
        return Err("Image file is empty (0 bytes)".into());
    }
    let source = sniff(&data).unwrap_or(extension_mime);
    let size = data.len();
    let original = |data: Vec<u8>, dimensions: Option<Dimensions>| Prepared {
        data,
        mime: source,
        original_size: size,
        dimensions,
        resized: false,
        compressed: false,
        original: true,
    };
    if source == "image/webp" {
        if !budget.fits(size) {
            return Err("WebP image exceeds the model image budget and the current image adapter cannot transcode WebP".into());
        }
        return Ok(original(data, None));
    }
    let image =
        image::load_from_memory(&data).map_err(|_| "Unable to decode image data".to_owned())?;
    let (width, height) = (image.width(), image.height());
    let unchanged = Dimensions {
        original_width: width,
        original_height: height,
        width,
        height,
    };
    if width <= MAX_DIMENSION && height <= MAX_DIMENSION && budget.fits(size) {
        return Ok(original(data, Some(unchanged)));
    }
    let Some(candidate) = budget.search(&image, source) else {
        return Err(format!(
            "Unable to compress image ({size} bytes) within the requested model image budget"
        ));
    };
    Ok(Prepared {
        resized: (candidate.width, candidate.height) != (width, height),
        compressed: candidate.data.len() < size || candidate.mime != source,
        dimensions: Some(Dimensions {
            width: candidate.width,
            height: candidate.height,
            ..unchanged
        }),
        data: candidate.data,
        mime: candidate.mime,
        original_size: size,
        original: false,
    })
}

#[cfg(test)]
#[path = "image_tests.rs"]
mod tests;
