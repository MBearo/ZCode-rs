//! 复刻 JS 字符串语义的小工具。

/// `String.prototype.trim` 使用的空白集合：WhiteSpace（含 Zs 类与 U+FEFF）加 LineTerminator。
/// 注意与 Rust `char::is_whitespace` 不同：包含 U+FEFF，不包含 U+0085。
pub(crate) fn is_js_whitespace(ch: char) -> bool {
    matches!(
        ch,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// JS `string.length`（UTF-16 码元数）。
pub(crate) fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// JS `trim()` 作用在字节串上；字节串可能在多字节字符中间被截断（unbash 在未闭合结构里
/// 按码元切片），此时按有损解码处理，只影响动态单词的文本，不影响安全判定。
pub(crate) fn trim_bytes(bytes: &[u8]) -> Vec<u8> {
    String::from_utf8_lossy(bytes)
        .trim_matches(is_js_whitespace)
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
