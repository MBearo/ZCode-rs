// 复制自 apps/zcode-cli-rust/crates/bash-parse/src/js.rs（07ab266d4b）；`crate::` 换成 `crate::parse::`。
//! 复刻 JS 字符串语义的小工具。

/// `String.prototype.trim` 使用的空白集合（WhiteSpace 加 LineTerminator：含 U+FEFF，不含 U+0085）与 JS
/// `string.length`：唯一定义在 base（F58：以前这里与 `readonly/js.rs` 各写一份）。
pub(crate) use rust_cli_base::js::string::{is_space, utf16_len};

/// JS `trim()` 作用在字节串上；字节串可能在多字节字符中间被截断（unbash 在未闭合结构里
/// 按码元切片），此时按有损解码处理，只影响动态单词的文本，不影响安全判定。
pub(crate) fn trim_bytes(bytes: &[u8]) -> Vec<u8> {
    String::from_utf8_lossy(bytes)
        .trim_matches(is_space)
        .as_bytes()
        .to_vec()
}

/// JS `source.slice(start, end)`：两端截断到长度内，`end < start` 时为空。
pub(crate) fn slice(source: &[u8], start: usize, end: usize) -> &[u8] {
    let end = end.min(source.len());
    let start = start.min(end);
    &source[start..end]
}

pub(crate) fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
