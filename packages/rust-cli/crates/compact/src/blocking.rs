//! 发请求前的阻塞线（docs/specs/rust-cli/compact.md §2.2）：到了阻塞线、又不交给上下文超限之后的压缩时，这一步不发
//! 请求，回合以错误结束。错误文字带上这一步自动压缩失败的原因。

use rust_cli_kernel_api::compaction::PROMPT_TOO_LONG;
use unicode_width::UnicodeWidthStr;

/// 失败原因的显示宽度上限。
const MAX_DETAIL_COLUMNS: usize = 300;
/// 压缩请求本身出错时失败说明的开头（`Failure::Error` 的显示文字，engine.rs）。
const ERROR_PREFIX: &str = "Error during compaction: ";

/// 按显示宽度截断：只取第一行，后面还有行时接 `…`；超过 `columns` 时截短、留一列给 `…`。
fn first_line(text: &str, columns: usize) -> String {
    let line = match text.split_once('\n') {
        Some((line, _)) => format!("{line}\u{2026}"),
        None => text.to_owned(),
    };
    if line.width() <= columns {
        return line;
    }
    let mut kept = String::new();
    let mut used = 0;
    for character in line.chars() {
        let width = unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
        if used + width > columns.saturating_sub(1) {
            break;
        }
        kept.push(character);
        used += width;
    }
    kept.push('\u{2026}');
    kept
}

/// 拦下时的错误文字。`failure` 是这一步自动压缩失败的说明：压缩请求出错、带着原因时接上原因。
pub(crate) fn text(failure: Option<&str>) -> String {
    match failure
        .and_then(|failure| failure.strip_prefix(ERROR_PREFIX))
        .filter(|detail| !detail.is_empty())
    {
        Some(detail) => format!(
            "{PROMPT_TOO_LONG} \u{b7} automatic compaction failed: {}",
            first_line(detail, MAX_DETAIL_COLUMNS)
        ),
        None => PROMPT_TOO_LONG.to_owned(),
    }
}

#[cfg(test)]
#[path = "blocking_tests.rs"]
mod tests;
