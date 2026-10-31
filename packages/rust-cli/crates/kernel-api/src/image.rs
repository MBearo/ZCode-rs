//! 发给模型的图片：文件头的格式判断、字节上限与扩展名（修复线 F58，README §4.1）。Read、TUI 的粘贴与拖入、剪贴板都用
//! 这里；以前 tools、tui、cli 各写一份，文件头的判断也不同。

use rust_cli_base::proto::mime;

/// 原样内联的上限（字节，base64 之前；不缩放，tools-files.md D10）。
pub const MAX_IMAGE_BYTES: usize = 3_932_160;

/// 当作图片的扩展名（小写，带点）。
pub const IMAGE_EXTENSIONS: [&str; 5] = [".png", ".jpg", ".jpeg", ".gif", ".webp"];

/// 认格式至少要的字节数。
const MIN_HEADER: usize = 4;
/// PNG 与 JPEG 的文件头。
const PNG_MAGIC: &[u8] = b"\x89PNG";
const JPEG_MAGIC: &[u8] = &[0xff, 0xd8, 0xff];
/// WebP：`RIFF` 之后跳过 4 字节的长度，这里是 `WEBP`。
const WEBP_TAG: std::ops::Range<usize> = 8..12;

/// 按文件头认格式：不足 4 字节不认；PNG 看前 4 字节，
/// JPEG 看前 3 字节，GIF 要 `GIF87a` / `GIF89a`，WebP 是 `RIFF????WEBP`。不是这四种时为 None。
pub fn media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() < MIN_HEADER {
        return None;
    }
    if bytes.starts_with(PNG_MAGIC) {
        Some(mime::PNG)
    } else if bytes.starts_with(JPEG_MAGIC) {
        Some(mime::JPEG)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(mime::GIF)
    } else if bytes.len() >= WEBP_TAG.end
        && bytes.starts_with(b"RIFF")
        && &bytes[WEBP_TAG] == b"WEBP"
    {
        Some(mime::WEBP)
    } else {
        None
    }
}

#[cfg(test)]
#[path = "image_tests.rs"]
mod tests;
