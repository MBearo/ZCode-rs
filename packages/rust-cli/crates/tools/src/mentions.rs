//! 用户输入里的 `@` 提及（docs/specs/rust-cli/tui-input.md §1.1）。这里只有纯函数：提取、行号、shell 引号、
//! attachment 的文字；读文件在 `user_input.rs`。

// `<system-reminder>` 包装只定义在 base（F58：以前这里手写一份）。
use rust_cli_base::reminder::wrap;
use rust_cli_kernel_api::message::{ATTACHMENT_BLOCKS, ContentBlock, ToolResultContent};
use rust_cli_kernel_api::user_input::InputAttachment;
use serde_json::{Value, json};

/// JS `\w`（不带 `u` 标志时只认 ASCII）。
fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// `(^|\s)` 之后的位置：`at` 是 `@` 的字节下标。
fn starts_mention(content: &str, at: usize) -> bool {
    at == 0
        || content[..at]
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace)
}

/// 带引号的提及的开头。
const QUOTED_OPEN: &str = "@\"";

/// `(^|\s)@"([^"]+)"`：带引号的提及；以 ` (agent)` 结尾的是 agent 提及，跳过。
fn quoted(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(offset) = content[from..].find(QUOTED_OPEN) {
        let at = from + offset;
        from = at + 1;
        if !starts_mention(content, at) {
            continue;
        }
        let body = &content[at + QUOTED_OPEN.len()..];
        let Some(end) = body.find('"') else { break };
        if end == 0 {
            continue;
        }
        let name = &body[..end];
        from = at + QUOTED_OPEN.len() + end + 1;
        if !name.ends_with(" (agent)") {
            out.push(name.to_owned());
        }
    }
    out
}

/// `(^|\s)@([^\s]+)\b`：`[^\s]+` 贪婪后回退到最近的 ASCII 单词边界；匹配不重叠。
fn regular(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(offset) = content[from..].find('@') {
        let at = from + offset;
        from = at + 1;
        if !starts_mention(content, at) {
            continue;
        }
        let body = &content[at + 1..];
        let run = body.find(char::is_whitespace).unwrap_or(body.len());
        let run = &body[..run];
        // 下一个字符（run 之后是空白或结尾，都不是单词字符）。
        let mut best = None;
        let chars: Vec<(usize, char)> = run.char_indices().collect();
        for (index, &(position, c)) in chars.iter().enumerate().rev() {
            let next = chars.get(index + 1).map(|&(_, next)| next);
            if is_word(c) != next.is_some_and(is_word) {
                best = Some(position + c.len_utf8());
                break;
            }
        }
        let Some(end) = best else { continue };
        let name = &run[..end];
        from = at + 1 + end;
        if !name.starts_with('"') {
            out.push(name.to_owned());
        }
    }
    out
}

/// 提及的文件名：引号的在前，普通的在后，去重保序。
pub fn extract(content: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in quoted(content).into_iter().chain(regular(content)) {
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// 提及里的行号范围：`file#L3-5` → (`file`, Some(3), Some(5))；`#L7` 只有一行；其他 `#…` 去掉。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mention {
    pub filename: String,
    pub offset: Option<u64>,
    pub limit: Option<u64>,
}

/// 提及的行号：`^([^#]+)(?:#L(\d+)(?:-(\d+))?)?(?:#[^#]*)?$`。
pub fn parse_lines(mention: &str) -> Mention {
    let whole = || Mention {
        filename: mention.to_owned(),
        offset: None,
        limit: None,
    };
    let (filename, rest) = match mention.find('#') {
        Some(0) => return whole(),
        Some(index) => (&mention[..index], &mention[index..]),
        None => (mention, ""),
    };
    let mut start = None;
    let mut end = None;
    let mut tail = rest;
    if let Some(lines) = rest.strip_prefix("#L") {
        let digits = lines.len() - lines.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits > 0 {
            start = lines[..digits].parse::<u64>().ok();
            tail = &lines[digits..];
            if let Some(after) = tail.strip_prefix('-') {
                let more =
                    after.len() - after.trim_start_matches(|c: char| c.is_ascii_digit()).len();
                if more > 0 {
                    end = after[..more].parse::<u64>().ok();
                    tail = &after[more..];
                }
            }
        }
    }
    // 剩下的只能是一个不含 `#` 的片段（`#heading`），否则整段当文件名。
    if !tail.is_empty() && (!tail.starts_with('#') || tail[1..].contains('#')) {
        return whole();
    }
    let end = end.or(start);
    let limit = match (start, end) {
        (Some(start), Some(end)) if start > 0 && end > 0 => Some((end + 1).saturating_sub(start)),
        _ => None,
    };
    Mention {
        filename: filename.to_owned(),
        offset: start,
        limit,
    }
}

/// shell 引号。
pub fn shell_quote(text: &str) -> String {
    if text.is_empty() {
        return "''".to_owned();
    }
    let safe = text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "_./:=@+,-".contains(c));
    if safe {
        return text.to_owned();
    }
    format!("'{}'", text.replace('\'', "'\"'\"'"))
}

/// 尖括号写成 `&lt;` `&gt;`，控制字符与 U+2028 / U+2029
/// 写成 `&#N;`。
fn escape_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}' | '\u{2028}' | '\u{2029}' => {
                out.push_str(&format!("&#{};", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// Read 的调用说明。
fn read_call(path: &str) -> String {
    let call = json!({ "file_path": path });
    format!("Called the Read tool with the following input: {call}")
}

/// `file` attachment：Read 的调用说明与结果。
pub fn file(path: &str, display: &str, content: &str) -> InputAttachment {
    InputAttachment {
        source: "file".to_owned(),
        text: wrap(&format!(
            "{}\nResult of calling the Read tool:\n{content}",
            read_call(path)
        )),
        data: json!({ "filename": path, "displayPath": display }),
    }
}

/// 数组形式的 Read 结果（notebook）的 `JSON.stringify`：块的键序是 `text, type`。
fn blocks_json(content: &[ToolResultContent]) -> String {
    let blocks = content
        .iter()
        .map(|block| match block {
            ToolResultContent::Text { text } => json!({ "text": text, "type": "text" }),
            ToolResultContent::Image { source } => json!({ "type": "image", "source": source }),
            ToolResultContent::Document { source } => {
                json!({ "type": "document", "source": source })
            }
            ToolResultContent::ToolReference { tool_name } => {
                json!({ "type": "tool_reference", "tool_name": tool_name })
            }
        })
        .collect();
    rust_cli_base::js::json::stringify(&Value::Array(blocks))
}

/// notebook 的 `file` attachment（docs/specs/rust-cli/tui-input.md §1.3）：结果里有图片时，
/// 内容块原样排在调用说明之后（文字块各自包装，存在 `data.blocks`）；否则结果是块数组的 JSON 文本。
pub fn notebook(path: &str, display: &str, content: &[ToolResultContent]) -> InputAttachment {
    let has_image = content
        .iter()
        .any(|block| matches!(block, ToolResultContent::Image { .. }));
    if !has_image {
        return file(path, display, &blocks_json(content));
    }
    media(path, display, content)
}

/// 图片的 `file` attachment：调用说明，之后是 Read 结果的内容块。
pub fn media(path: &str, display: &str, content: &[ToolResultContent]) -> InputAttachment {
    let blocks: Vec<ContentBlock> = content
        .iter()
        .filter_map(|block| match block {
            ToolResultContent::Text { text } => Some(ContentBlock::Text { text: wrap(text) }),
            ToolResultContent::Image { source } => Some(ContentBlock::Image {
                source: source.clone(),
            }),
            // 提及不附 PDF 内容（tui-input.md §1.3），Read 的 notebook 与图片结果里也没有 document。
            ToolResultContent::Document { .. } | ToolResultContent::ToolReference { .. } => None,
        })
        .collect();
    InputAttachment {
        source: "file".to_owned(),
        text: wrap(&read_call(path)),
        data: json!({ "filename": path, "displayPath": display, ATTACHMENT_BLOCKS: blocks }),
    }
}

/// 小 PDF 的 `file` attachment：只说读过（渲染时把 base64 置空，不附 document 块）。
pub fn pdf(path: &str, display: &str, size: &str) -> InputAttachment {
    file(path, display, &format!("PDF file read: {path} ({size})"))
}

/// 提及 PDF 时按大小估页数（没有 pdfinfo 时每页约 100KB），超过这么多页给引用。
pub const PDF_PAGE_BYTES: u64 = 102_400;
pub const PDF_INLINE_PAGES: u64 = 10;

/// `pdf_reference`（页数未知）：太长，不附。
pub fn pdf_reference(path: &str, display: &str, size: u64, size_text: &str) -> InputAttachment {
    InputAttachment {
        source: "pdf_reference".to_owned(),
        text: wrap(&format!(
            "PDF file: {} (page count unknown, {size_text}). It was not attached because it may be too long. Use the Read tool with the pages parameter to read specific page ranges (e.g., pages: \"1-5\"). Start by reading the first few pages to understand the structure, then read more as needed. Maximum 20 pages per request.",
            escape_path(path)
        )),
        data: json!({ "filename": path, "pageCount": null, "fileSize": size, "displayPath": display }),
    }
}

/// `directory` attachment：一次 `ls` 的调用说明与结果。
pub fn directory(path: &str, display: &str, names: &[String]) -> InputAttachment {
    let call = json!({
        "command": format!("ls {}", shell_quote(path)),
        "description": format!("Lists files in {path}"),
    });
    InputAttachment {
        source: "directory".to_owned(),
        text: wrap(&format!(
            "Called the Bash tool with the following input: {call}\nResult of calling the Bash tool:\n{}",
            names.join("\n")
        )),
        data: json!({ "path": path, "displayPath": display }),
    }
}

/// `at_mention_reference`（`unread: too_large`）：文件太大，不附内容。
pub fn too_large(path: &str, size: &str) -> InputAttachment {
    InputAttachment {
        source: "at_mention_reference".to_owned(),
        text: wrap(&format!(
            "The user @-mentioned {} ({size}). Its contents were not attached because the file is too large to read all at once, and a Read call with no limit parameter will fail. Read it in portions with the offset and limit parameters, starting with a few hundred lines, or search for specific content instead of reading the whole file.",
            escape_path(path)
        )),
        data: json!({ "mentions": [path], "unread": "too_large" }),
    }
}

/// 目录提及最多看这么多项，最多列出这么多。
pub const DIRECTORY_SCAN: usize = 10_000;
pub const DIRECTORY_LIST: usize = 1_000;

/// 目录的条目名：按名字排序（D6），前 [`DIRECTORY_SCAN`] 项里取前 [`DIRECTORY_LIST`] 项，超出时说明剩下几项。
pub fn directory_names(mut names: Vec<String>) -> Vec<String> {
    names.sort();
    let total = names.len();
    let scanned = total.min(DIRECTORY_SCAN);
    let shown = scanned.min(DIRECTORY_LIST);
    let mut out: Vec<String> = names.into_iter().take(shown).collect();
    if shown < total {
        out.push(format!("\u{2026} and {} more entries", total - shown));
    }
    out
}

#[cfg(test)]
#[path = "mentions_tests.rs"]
mod tests;
