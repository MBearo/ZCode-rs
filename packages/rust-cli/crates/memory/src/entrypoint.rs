//! `MEMORY.md` 索引的截断（docs/specs/rust-cli/memory.md §3）。长度与下标都按 UTF-16 码元（JS 字符串语义）。

// 截断提示的大小写法与 `<persisted-output>` 是同一个函数（`formatBytes`）。
// 缺陷修复（F58）：以前这里自己写了一份，一位小数用 Rust 的 `{:.1}`（平分时取偶，25856 码元得 `25.2KB`）；
// JS `toFixed(1)` 平分时取大（`25.3KB`）。改用 kernel-api 的唯一实现。
use rust_cli_kernel_api::persisted_output::format_bytes;

/// 索引文件名。
pub const ENTRYPOINT: &str = "MEMORY.md";
/// 最多载入的行数。
pub const MAX_LINES: usize = 200;
/// 最多载入的长度（UTF-16 码元）。
pub const MAX_CHARS: usize = 25_000;

const NEWLINE: u16 = b'\n' as u16;
/// 截断说明里引用的下一行最多的长度（UTF-16 码元）。
const NEXT_LINE_PREVIEW_CHARS: usize = 80;

/// 超长时截到最后一个完整的词（太短就硬截，上限 80），加省略号。
fn shorten(text: &[u16], max: usize) -> String {
    if text.len() <= max {
        return String::from_utf16_lossy(text);
    }
    let cut = String::from_utf16_lossy(&text[..max - 1]);
    let word_end = cut
        .char_indices()
        .rev()
        .find(|(index, c)| {
            c.is_whitespace() && !cut[index + c.len_utf8()..].contains(char::is_whitespace)
        })
        .map(|(index, _)| cut[..index].trim_end().to_owned())
        .unwrap_or_default();
    let chosen = if word_end.encode_utf16().count() > max / 2 {
        word_end
    } else {
        cut.trim_end().to_owned()
    };
    format!("{chosen}\u{2026}")
}

fn count_newlines(units: &[u16]) -> usize {
    units.iter().filter(|unit| **unit == NEWLINE).count()
}

/// 截断后的内容；不超限时就是 trim 之后的原文。
pub fn truncate(raw: &str) -> String {
    let trimmed: Vec<u16> = raw.trim().encode_utf16().collect();
    let line_count = count_newlines(&trimmed) + 1;
    let length = trimmed.len();
    let too_many_lines = line_count > MAX_LINES;
    let too_long = length > MAX_CHARS;
    if !too_many_lines && !too_long {
        return String::from_utf16_lossy(&trimmed);
    }
    let mut kept: &[u16] = &trimmed;
    if too_many_lines {
        let end = trimmed
            .iter()
            .enumerate()
            .filter(|(_, unit)| **unit == NEWLINE)
            .nth(MAX_LINES - 1)
            .map_or(trimmed.len(), |(index, _)| index);
        kept = &trimmed[..end];
    }
    if kept.len() > MAX_CHARS {
        // JS `lastIndexOf("\n", 25000)`：下标不超过 25000 的最后一个换行。
        let cut = kept[..=MAX_CHARS]
            .iter()
            .rposition(|unit| *unit == NEWLINE)
            .filter(|index| *index > 0)
            .unwrap_or(MAX_CHARS);
        kept = &kept[..cut];
    }
    let kept_lines = if trimmed.get(kept.len()) == Some(&NEWLINE) {
        count_newlines(kept) + 1
    } else {
        0
    };
    let next_start = (kept.len() + 1).min(trimmed.len());
    let next_end = trimmed[next_start..]
        .iter()
        .position(|unit| *unit == NEWLINE)
        .map_or(trimmed.len(), |offset| next_start + offset);
    let next_line: Vec<u16> = String::from_utf16_lossy(&trimmed[next_start..next_end])
        .trim()
        .encode_utf16()
        .collect();
    let position = if kept_lines == 0 {
        format!(
            "everything after the first {} characters of line 1 was cut off",
            kept.len()
        )
    } else {
        let next = if next_line.is_empty() {
            String::new()
        } else {
            format!(" (\"{}\")", shorten(&next_line, NEXT_LINE_PREVIEW_CHARS))
        };
        format!(
            "{} of {line_count} lines were cut off, starting at line {}{next}",
            line_count - kept_lines,
            kept_lines + 1
        )
    };
    let reason = match (too_long, too_many_lines) {
        (true, false) => format!(
            "{} (limit: {}) \u{2014} index entries are too long",
            format_bytes(length as u64),
            format_bytes(MAX_CHARS as u64)
        ),
        (false, true) => format!("{line_count} lines (limit: {MAX_LINES})"),
        _ => format!("{line_count} lines and {}", format_bytes(length as u64)),
    };
    format!(
        "{}\n\n> WARNING: {ENTRYPOINT} is {reason}. Only part of it was loaded: {position}. Keep index entries to one line under ~200 chars; move detail into topic files.",
        String::from_utf16_lossy(kept)
    )
}

#[cfg(test)]
#[path = "entrypoint_tests.rs"]
mod tests;
