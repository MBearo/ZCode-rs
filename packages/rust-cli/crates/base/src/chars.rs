//! 不可见字符的判断（docs/specs/rust-cli/constants.md §2、§5.1.4）：控制字符、行与段分隔符、双向控制符与零宽字符，
//! 以及给提示词用的 `&#N;` 转义。以前 context、tools、policy、session、ext 各写各的范围（F122 收拢，调用方逐批改过来）。

use std::ops::RangeInclusive;

/// C0 控制字符（U+0000–U+001F）。
const C0: RangeInclusive<u32> = 0x00..=0x1f;
/// DEL 与 C1 控制字符（U+007F–U+009F）。
const DEL_AND_C1: RangeInclusive<u32> = 0x7f..=0x9f;
/// 行分隔符与段分隔符。
pub const LINE_SEPARATOR: char = '\u{2028}';
pub const PARAGRAPH_SEPARATOR: char = '\u{2029}';

/// 双向控制符与零宽字符：阿拉伯字母标记 U+061C；零宽空格、零宽不连字、零宽连字、LRM、RLM（U+200B–U+200F）；
/// LRE、RLE、PDF、LRO、RLO（U+202A–U+202E）；LRI、RLI、FSI、PDI 与已弃用的格式字符（U+2066–U+206F）；BOM（U+FEFF）。
pub const BIDI_AND_ZERO_WIDTH: [RangeInclusive<u32>; 5] = [
    0x061c..=0x061c,
    0x200b..=0x200f,
    0x202a..=0x202e,
    0x2066..=0x206f,
    0xfeff..=0xfeff,
];

/// C0 控制字符。
pub fn is_c0(c: char) -> bool {
    C0.contains(&u32::from(c))
}

/// DEL 或 C1 控制字符。
pub fn is_del_or_c1(c: char) -> bool {
    DEL_AND_C1.contains(&u32::from(c))
}

/// U+2028 或 U+2029。
pub fn is_line_break_separator(c: char) -> bool {
    c == LINE_SEPARATOR || c == PARAGRAPH_SEPARATOR
}

/// 进提示词前要写成 `&#N;` 的字符：C0、DEL 与 C1、U+2028 / U+2029。
pub fn is_entity_escaped(c: char) -> bool {
    is_c0(c) || is_del_or_c1(c) || is_line_break_separator(c)
}

/// [`is_entity_escaped`] 的字符换成 `&#N;`（十进制码点），其余原样。
pub fn escape_entities(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if is_entity_escaped(c) {
            escaped.push_str(&format!("&#{};", u32::from(c)));
        } else {
            escaped.push(c);
        }
    }
    escaped
}

/// 双向控制符或零宽字符（[`BIDI_AND_ZERO_WIDTH`]）。
pub fn is_bidi_or_zero_width(c: char) -> bool {
    let code = u32::from(c);
    BIDI_AND_ZERO_WIDTH
        .iter()
        .any(|range| range.contains(&code))
}

#[cfg(test)]
#[path = "chars_tests.rs"]
mod tests;
