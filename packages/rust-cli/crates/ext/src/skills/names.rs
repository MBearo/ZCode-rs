//! skill 名字的两种校验，只定义在这里（修复线
//! F58，README §4.1）。两个都按 JS 的 `length`（UTF-16 码元）限 256。以前列表的那份按码点计数、用 Rust 的
//! `is_alphanumeric`（组合标记算非法、Ⓐ 这类符号算合法）。

use std::sync::OnceLock;

use regex::Regex;

/// 名字的长度上限（JS `length`）。
const MAX_UNITS: usize = 256;

fn short(name: &str) -> bool {
    name.encode_utf16().count() <= MAX_UNITS
}

/// 能写进未知 skill 提醒的名字：`/^[A-Za-z0-9_:.-]+$/`。
pub(crate) fn nameable(name: &str) -> bool {
    short(name)
        && !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '.' | '-'))
}

/// 清单里能用的显示名：`/^[\p{L}\p{M}\p{N}_:.-]+$/u`。
pub(crate) fn valid_display(name: &str) -> bool {
    static PATTERN: OnceLock<Option<Regex>> = OnceLock::new();
    short(name)
        && PATTERN
            .get_or_init(|| Regex::new(r"^[\p{L}\p{M}\p{N}_:.\-]+$").ok())
            .as_ref()
            .is_some_and(|pattern| pattern.is_match(name))
}
