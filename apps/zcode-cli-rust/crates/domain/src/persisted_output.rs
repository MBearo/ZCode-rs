//! Node `result-persistence-format.ts`: the `<persisted-output>` envelope and
//! its byte formatters (JS `toFixed(1)` reproduced with integer arithmetic).
use crate::js_string::utf16_prefix;

const OPEN: &str = "<persisted-output>";
const CLOSE: &str = "</persisted-output>";

/// Node `formatPersistedOutputEnvelope`.
pub fn envelope(
    content: &str,
    original_bytes: u64,
    path: &str,
    preview_chars: usize,
    format: fn(u64) -> String,
) -> String {
    let (preview, more) = preview_first_chars(content, preview_chars);
    let mut lines = vec![
        OPEN.to_owned(),
        format!(
            "Output too large ({}). Full output saved to: {path}",
            format(original_bytes)
        ),
        String::new(),
        format!("Preview (first {}):", format(preview_chars as u64)),
        preview.to_owned(),
    ];
    if more {
        lines.push("...".into());
    }
    lines.push(CLOSE.into());
    lines.join("\n")
}

/// Node `previewFirstChars`: whole content when short, else cut at the last
/// newline inside the first `max` UTF-16 units when it lies past the middle.
fn preview_first_chars(content: &str, max: usize) -> (&str, bool) {
    if content.encode_utf16().count() <= max {
        return (content, false);
    }
    let head = utf16_prefix(content, max);
    let newline = head
        .rfind('\n')
        .filter(|at| head[..*at].encode_utf16().count() * 2 > max);
    (newline.map_or(head, |at| &head[..at]), true)
}

/// Node result budget `strategy: "truncate"`: the head of `text` and a note,
/// together at most `max` bytes.
pub fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let suffix = format!(
        "\n\n[Tool output truncated by resultBudget: originalBytes={}, maxModelBytes={max}, strategy=truncate]",
        text.len()
    );
    let mut end = max.saturating_sub(suffix.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{suffix}", &text[..end])
}

/// JS `(numerator / denominator).toFixed(1)` without a trailing `.0`, for
/// non-negative integers (ties round up, like the exact JS algorithm).
fn tenths(numerator: u64, denominator: u64) -> String {
    let n = (u128::from(numerator) * 10 + u128::from(denominator) / 2) / u128::from(denominator);
    if n % 10 == 0 {
        (n / 10).to_string()
    } else {
        format!("{}.{}", n / 10, n % 10)
    }
}

/// Node `formatBashOutputByteSize` / `formatCompactFileSize`: 1024-based, no space.
pub fn compact_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    match bytes {
        0 => "0 bytes".into(),
        b if b < KB => format!("{b} bytes"),
        b if b < KB * KB => format!("{}KB", tenths(b, KB)),
        b if b < KB * KB * KB => format!("{}MB", tenths(b, KB * KB)),
        b => format!("{}GB", tenths(b, KB * KB * KB)),
    }
}

/// Node `formatDecimalBytes` (generic envelope).
pub fn decimal_bytes(bytes: u64) -> String {
    let round = |unit: u64| (bytes + unit / 2) / unit;
    match bytes {
        b if b < 1_000 => format!("{b} B"),
        b if b < 1_000_000 => format!("{} KB", round(1_000)),
        b if b < 1_000_000_000 => format!("{} MB", round(1_000_000)),
        _ => format!("{} GB", round(1_000_000_000)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_bytes_like_node() {
        assert_eq!(compact_bytes(0), "0 bytes");
        assert_eq!(compact_bytes(1023), "1023 bytes");
        assert_eq!(compact_bytes(2000), "2KB");
        // 46336 / 1024 = 45.25：JS toFixed 取较大者。
        assert_eq!(compact_bytes(46336), "45.3KB");
        assert_eq!(compact_bytes(1_572_864), "1.5MB");
        assert_eq!(decimal_bytes(999), "999 B");
        assert_eq!(decimal_bytes(1_500), "2 KB");
        assert_eq!(decimal_bytes(20_000_000), "20 MB");
    }

    #[test]
    fn previews_cut_at_a_late_newline() {
        let content = format!("{}\n{}", "a".repeat(1500), "b".repeat(1000));
        let text = envelope(&content, 30_001, "/p", 2000, compact_bytes);
        assert_eq!(
            text,
            format!(
                "<persisted-output>\nOutput too large (29.3KB). Full output saved to: /p\n\nPreview (first 2KB):\n{}\n...\n</persisted-output>",
                "a".repeat(1500)
            )
        );
        let early = format!("{}\n{}", "a".repeat(10), "b".repeat(3000));
        let cut = preview_first_chars(&early, 2000);
        assert_eq!(cut.0.len(), 2000);
        assert_eq!(preview_first_chars("short", 2000), ("short", false));
    }
}
