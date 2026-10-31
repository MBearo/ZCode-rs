//! 按行选取文本（读文件的内存路径）。纯函数，没有 IO。

/// 开头的 BOM（U+FEFF）在 UTF-8 里的字节数。
const BOM_BYTES: u64 = '\u{feff}'.len_utf8() as u64;

/// 选取结果；字段与 `readFileInRange` 的返回值同名。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Selected {
    pub content: String,
    pub line_count: u64,
    pub total_lines: u64,
    pub total_bytes: u64,
}

/// `text` 是整份文件的 UTF-8 有损解码结果，`total_bytes` 是文件字节数。
///
/// - 开头的 BOM 去掉，`total_bytes` 减 3；
/// - 从头读到尾（`start_line == 0` 且不限行数）时：`\r\n` 换成 `\n`，末尾单独的 `\r` 去掉，
///   行数是 `\n` 的个数加一；
/// - 否则按 `\n` 切行，每行去掉末尾的一个 `\r`，选取 `[start_line, start_line + max_lines)`。
pub(crate) fn select(
    text: &str,
    total_bytes: u64,
    start_line: u64,
    max_lines: Option<u64>,
) -> Selected {
    let (text, total_bytes) = match text.strip_prefix('\u{feff}') {
        Some(rest) => (rest, total_bytes.saturating_sub(BOM_BYTES)),
        None => (text, total_bytes),
    };
    if start_line == 0 && max_lines.is_none() {
        let mut content = if text.contains('\r') {
            text.replace("\r\n", "\n")
        } else {
            text.to_owned()
        };
        if content.ends_with('\r') {
            content.pop();
        }
        let lines = content.matches('\n').count() as u64 + 1;
        return Selected {
            content,
            line_count: lines,
            total_lines: lines,
            total_bytes,
        };
    }
    let end = max_lines.map_or(u64::MAX, |limit| start_line.saturating_add(limit));
    let mut selected = Vec::new();
    let mut total_lines = 0_u64;
    for line in text.split('\n') {
        if total_lines >= start_line && total_lines < end {
            selected.push(line.strip_suffix('\r').unwrap_or(line));
        }
        total_lines += 1;
    }
    Selected {
        content: selected.join("\n"),
        line_count: selected.len() as u64,
        total_lines,
        total_bytes,
    }
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
