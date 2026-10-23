//! 大内容落盘与 `<persisted-output>` 预览（docs/specs/rust-cli/hooks.md §4.5、tools-files.md D5）。
//!
//! 工具结果与 hook 输出共用同一套落盘与预览，两边都只经 [`persist`]：全文经
//! [`ArtifactStore`] 落盘（README §6 规则 10），交给模型的是位置、原大小与前 2000 个 UTF-16 码元。位置不是
//! 本机路径，而是 `zcode-artifact://<会话>/<artifact>`（云端也能用，Read 能读）。

use rust_cli_base::js::string::{utf16_len, utf16_prefix};

use crate::ids::SessionId;
use crate::store::{ArtifactPurpose, ArtifactStore, StoreError};

/// 落盘位置的前缀。
pub const ARTIFACT_SCHEME: &str = "zcode-artifact://";
/// 预览的开头与结尾。
pub const OPEN: &str = "<persisted-output>";
pub const CLOSE: &str = "</persisted-output>";
/// 预览长度，按 UTF-16 码元。
pub const PREVIEW_UNITS: usize = 2_000;

fn fixed1(value: f64) -> String {
    // Number.prototype.toFixed(1) 再去掉 ".0"；字节数除以 1024 的幂是二进制精确值，乘 10 不丢精度。
    let tenths = (value * 10.0).round() as u64;
    match tenths % 10 {
        0 => (tenths / 10).to_string(),
        digit => format!("{}.{digit}", tenths / 10),
    }
}

/// 字节数的写法：不到 1 KB 写 `N bytes`，否则 KB / MB / GB 各一位小数。
pub fn format_bytes(bytes: u64) -> String {
    let kilobytes = bytes as f64 / 1024.0;
    if kilobytes < 1.0 {
        return format!("{bytes} bytes");
    }
    if kilobytes < 1024.0 {
        return format!("{}KB", fixed1(kilobytes));
    }
    let megabytes = kilobytes / 1024.0;
    if megabytes < 1024.0 {
        return format!("{}MB", fixed1(megabytes));
    }
    format!("{}GB", fixed1(megabytes / 1024.0))
}

/// 预览：在前 2000 个码元里最后一个换行处截断（换行不过一半时直接截）；第二项是“有截断”。
pub fn preview(text: &str) -> (&str, bool) {
    if utf16_len(text) <= PREVIEW_UNITS {
        return (text, false);
    }
    let head = utf16_prefix(text, PREVIEW_UNITS);
    let cut = match head.rfind('\n') {
        Some(at) if utf16_len(&head[..at]) > PREVIEW_UNITS / 2 => &head[..at],
        _ => head,
    };
    (cut, true)
}

/// 给模型的文字：`original` 是原文的 UTF-16 长度（按 `length` 计，标成字节）。
pub fn message(location: &str, original: usize, preview: &str, more: bool) -> String {
    format!(
        "{OPEN}\nOutput too large ({}). Full output saved to: {location}\n\nPreview (first {}):\n{preview}{}{CLOSE}",
        format_bytes(u64::try_from(original).unwrap_or(u64::MAX)),
        format_bytes(PREVIEW_UNITS as u64),
        if more { "\n...\n" } else { "\n" }
    )
}

/// 把 `body` 经 [`ArtifactStore`] 落盘（用途 `ToolOutput`，放在会话的 tool-results 里），返回给模型的
/// `<persisted-output>` 文字。存不下时返回存储的错误，由调用方按自己的规则退回。
pub async fn persist(
    artifacts: &dyn ArtifactStore,
    session: &SessionId,
    body: &str,
    media_type: &str,
) -> Result<String, StoreError> {
    let artifact = artifacts
        .put(
            session,
            ArtifactPurpose::ToolOutput,
            media_type.to_owned(),
            body.as_bytes().to_vec(),
        )
        .await?;
    let location = format!("{ARTIFACT_SCHEME}{}/{}", artifact.session, artifact.id);
    let (shown, more) = preview(body);
    Ok(message(&location, utf16_len(body), shown, more))
}

#[cfg(test)]
#[path = "persisted_output_tests.rs"]
mod tests;
