//! 粘贴的图片（docs/specs/rust-cli/tui-input.md §3.1）：图片按粘贴编号（与折叠的
//! 粘贴文字共用一个计数）放进草稿，光标处插入 `[Image #N]`；发送时输入框里还在的占位文字对应的图片按编号排在
//! 文字块之后。
use base64::Engine;
use rust_cli_kernel_api::message::{ContentBlock, ImageSource};

use super::{Composer, InputMode};

/// 一张粘贴的图片（base64）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pasted {
    pub id: usize,
    pub placeholder: String,
    pub media_type: String,
    pub data: String,
}

/// 发给模型的图片上限（base64 之前，与 Read 的图片上限相同；不缩放，D10）：kernel-api 的一个常量（F58）。
pub const MAX_IMAGE_BYTES: usize = rust_cli_kernel_api::image::MAX_IMAGE_BYTES;

/// 按文件头认图片格式（png、jpeg、gif、webp）；不是图片时为 None。与 Read 同一个判断（kernel-api
/// `image::media_type`）。修复线 F58：以前这里要完整的 8 字节 PNG 签名，Read 只看 4 字节。
pub fn media_type(bytes: &[u8]) -> Option<&'static str> {
    rust_cli_kernel_api::image::media_type(bytes)
}

/// 粘贴的文字是一个图片文件的路径时（拖文件进终端）给出路径：去掉首尾空白与引号、
/// `file://`，`\ ` 还原成空格；扩展名是 png、jpg、jpeg、gif、webp（不分大小写）。
pub fn image_path(text: &str) -> Option<String> {
    let trimmed = text.trim().trim_matches(['\'', '"']);
    if trimmed.is_empty() || trimmed.contains('\n') {
        return None;
    }
    let path = trimmed
        .strip_prefix("file://")
        .unwrap_or(trimmed)
        .replace("\\ ", " ");
    let lower = path.to_ascii_lowercase();
    rust_cli_kernel_api::image::IMAGE_EXTENSIONS
        .iter()
        .any(|extension| lower.ends_with(extension))
        .then_some(path)
}

/// 加图片的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Added {
    /// 插入了这段占位文字。
    Inserted(String),
    /// 文件头不是支持的图片。
    NotImage,
    /// 超过 [`MAX_IMAGE_BYTES`]。
    TooLarge,
}

impl Composer {
    /// 在光标处加一张图片（粘贴图片回到普通模式）。
    pub fn add_image(&mut self, bytes: &[u8]) -> Added {
        let Some(media_type) = media_type(bytes) else {
            return Added::NotImage;
        };
        if bytes.len() > MAX_IMAGE_BYTES {
            return Added::TooLarge;
        }
        self.paste_count += 1;
        let placeholder = format!("[Image #{}]", self.paste_count);
        self.mode = InputMode::Prompt;
        // 缺陷修复（修复线 F13）：原来插入 `[Image #N] `（带尾空格），接着键入空格时成了两个空格。
        // 插入时不带尾空格，紧跟上一张图片时前面补一个；之后的空格由 `before_typing` 按规则补。
        let lead = if self.after_image { " " } else { "" };
        self.area.insert_str(&format!("{lead}{placeholder}"));
        self.after_image = true;
        self.images.push(Pasted {
            id: self.paste_count,
            placeholder: placeholder.clone(),
            media_type: media_type.to_owned(),
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
        });
        Added::Inserted(placeholder)
    }

    /// 键入一个字符之前：紧跟在粘贴的图片后面、没有按 alt、字符不是空白也不是 `.,?!:;)]` 之一时
    /// 先补一个空格。不论补不补，图片的标记都清掉。
    pub fn before_typing(&mut self, c: char, alt: bool) {
        if std::mem::take(&mut self.after_image)
            && !alt
            && !c.is_whitespace()
            && !".,?!:;)]".contains(c)
        {
            self.area.insert(' ');
        }
    }

    /// 有附上的图片：占位文字还在输入框里的（运行中的投递方式按它判断，tui-input.md D9）。
    pub fn has_images(&self) -> bool {
        let text = self.area.text();
        self.images
            .iter()
            .any(|image| text.contains(&image.placeholder))
    }

    /// 编辑之后（修复线 F26，每次编辑后同步删掉图片）：占位文字不在了的图片去掉，留给撤销；
    /// `undo` 为真（ctrl+_）时，占位文字回来了的图片一起回来。
    /// ctrl+y 粘回、手打同样的文字都不找回。
    pub fn sync_images(&mut self, undo: bool) {
        let text = self.area.text().to_owned();
        let (kept, gone): (Vec<_>, Vec<_>) = std::mem::take(&mut self.images)
            .into_iter()
            .partition(|image| text.contains(&image.placeholder));
        self.images = kept;
        self.detached.extend(gone);
        if undo {
            let (back, still): (Vec<_>, Vec<_>) = std::mem::take(&mut self.detached)
                .into_iter()
                .partition(|image| text.contains(&image.placeholder));
            self.detached = still;
            self.images.extend(back);
            self.images.sort_by_key(|image| image.id);
        }
    }

    /// 发送时的图片块：占位文字还在 `text` 里的那些，按编号（排在文字块之后）。
    pub fn image_blocks(&self, text: &str) -> Vec<ContentBlock> {
        let mut images: Vec<&Pasted> = self
            .images
            .iter()
            .filter(|image| text.contains(&image.placeholder))
            .collect();
        images.sort_by_key(|image| image.id);
        images
            .into_iter()
            .map(|image| ContentBlock::Image {
                source: ImageSource::Base64 {
                    media_type: image.media_type.clone(),
                    data: image.data.clone(),
                },
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "composer_images_tests.rs"]
mod tests;
