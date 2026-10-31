//! Read 的检查与格式化：二进制 / 设备文件判定、行号、token 估算、图片类型。

use rust_cli_base::tokens;
use rust_cli_kernel_api::tool::ToolError;

use crate::common::failed;
use crate::texts;

/// 不加 limit 时允许读的最大字节数。
pub(crate) const MAX_BYTES: u64 = 262_144;
/// 一次读取允许的最大 token 数。
const MAX_TOKENS: u64 = 25_000;
/// 图片原样内联的上限（字节）：kernel-api 的一个常量，TUI 的粘贴也用它（F58）。
pub(crate) const MAX_IMAGE_BYTES: usize = rust_cli_kernel_api::image::MAX_IMAGE_BYTES;

/// 读出来就是二进制、不能当文本读的扩展名。
const BINARY: &[&str] = &[
    ".png", ".jpg", ".jpeg", ".gif", ".bmp", ".ico", ".webp", ".tiff", ".tif", ".mp4", ".mov",
    ".avi", ".mkv", ".webm", ".wmv", ".flv", ".m4v", ".mpeg", ".mpg", ".mp3", ".wav", ".ogg",
    ".flac", ".aac", ".m4a", ".wma", ".aiff", ".opus", ".zip", ".tar", ".gz", ".bz2", ".7z",
    ".rar", ".xz", ".z", ".tgz", ".iso", ".exe", ".dll", ".so", ".dylib", ".bin", ".o", ".a",
    ".obj", ".lib", ".app", ".msi", ".deb", ".rpm", ".pdf", ".doc", ".docx", ".xls", ".xlsx",
    ".ppt", ".pptx", ".odt", ".ods", ".odp", ".ttf", ".otf", ".woff", ".woff2", ".eot", ".pyc",
    ".pyo", ".class", ".jar", ".war", ".ear", ".node", ".wasm", ".rlib", ".sqlite", ".sqlite3",
    ".db", ".mdb", ".idx", ".psd", ".ai", ".eps", ".sketch", ".fig", ".xd", ".blend", ".3ds",
    ".max", ".swf", ".fla", ".lockb", ".dat", ".data",
];
pub(crate) const IMAGES: &[&str] = &rust_cli_kernel_api::image::IMAGE_EXTENSIONS;
/// 会阻塞或无限输出的设备文件。
const DEVICES: &[&str] = &[
    "/dev/zero",
    "/dev/random",
    "/dev/urandom",
    "/dev/full",
    "/dev/stdin",
    "/dev/tty",
    "/dev/console",
    "/dev/stdout",
    "/dev/stderr",
    "/dev/fd/0",
    "/dev/fd/1",
    "/dev/fd/2",
];

/// 扩展名在二进制表里（含图片与 PDF）。扩展名取最后一个点之后（含点），不看是不是文件名的开头。
pub(crate) fn has_binary_extension(path: &str) -> bool {
    let extension = path
        .rfind('.')
        .map_or_else(String::new, |at| path[at..].to_lowercase());
    BINARY.contains(&extension.as_str())
}

/// 二进制扩展名，除去图片与 PDF（它们另有读法）。
pub(crate) fn is_binary(path: &str) -> bool {
    let extension = path
        .rfind('.')
        .map_or_else(String::new, |at| path[at..].to_lowercase());
    has_binary_extension(path) && !IMAGES.contains(&extension.as_str()) && extension != ".pdf"
}

pub(crate) fn is_device(path: &str) -> bool {
    if DEVICES.contains(&path) {
        return true;
    }
    if let Some(rest) = path.strip_prefix("/proc/") {
        if path.ends_with("/fd/0") || path.ends_with("/fd/1") || path.ends_with("/fd/2") {
            return true;
        }
        let mut parts = rest.split('/');
        let (_pid, leaf, extra) = (parts.next(), parts.next(), parts.next());
        return extra.is_none()
            && leaf.is_some_and(|leaf| {
                ["environ", "cmdline", "auxv", "maps", "mem", "stat"].contains(&leaf)
            });
    }
    false
}

/// `<行号>\t<内容>`，每行去掉末尾的 `\r`。
pub(crate) fn number_lines(content: &str, start: u64) -> String {
    content
        .split('\n')
        .enumerate()
        .map(|(index, line)| {
            format!(
                "{}\t{}",
                start + index as u64,
                line.strip_suffix('\r').unwrap_or(line)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 粗估 token 数：JSON 类每 2 个字符一个 token，其余 4 个。
fn estimate_tokens(content: &str, extension: &str) -> u64 {
    // 每 token 的字符数与 Math.round 只定义在 base 的 `tokens`。
    let per_token = if matches!(extension, ".json" | ".jsonl" | ".jsonc") {
        tokens::JSON_CHARS_PER_TOKEN
    } else {
        tokens::CHARS_PER_TOKEN
    };
    tokens::estimate(crate::common::utf16_len(content) as u64, per_token)
}

/// 超过上限时的报错。没有 count_tokens 时也用这个估算值。
pub(crate) fn check_tokens(content: &str, extension: &str) -> Result<(), ToolError> {
    let tokens = estimate_tokens(content, extension);
    if tokens > MAX_TOKENS {
        return Err(failed(texts::too_many_tokens(tokens, MAX_TOKENS)));
    }
    Ok(())
}

/// 文件头的格式（kernel-api `image::media_type`）。修复线 F58：以前自己判断，`GIF8` 开头就算 GIF、不足 4 字节
/// 也认，与 TUI 的判断不同。
pub(crate) fn image_type(bytes: &[u8]) -> Option<&'static str> {
    rust_cli_kernel_api::image::media_type(bytes)
}

#[cfg(test)]
#[path = "read_tests.rs"]
mod tests;
