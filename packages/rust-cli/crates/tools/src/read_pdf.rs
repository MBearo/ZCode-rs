//! Read 读 PDF（docs/specs/rust-cli/tools-files.md §1.5，修复线 F24）：PDF 分支、页码语法、整读与取页的报错分类。
//! pdfinfo 与 pdftoppm 在执行环境里跑（kernel-api `env_pdf`），这里只拼给模型的文字。
//!
//! 修 bug（F24）：以前没有这个分支，PDF 按文本有损解码成乱码，`pages` 被忽略；`@` 提及长 PDF 时
//! `pdf_reference` 让模型用 `pages` 读，照做也读不出页面。

use std::future::Future;

use rust_cli_base::js::string::is_space;
use rust_cli_kernel_api::env::{EnvError, EnvPath, ReadFile};
use rust_cli_kernel_api::env_files::{FileKind, FileStat};
use rust_cli_kernel_api::env_pdf::{PdfPageCount, PdfRender, RenderPdfPages};
use rust_cli_kernel_api::message::{ImageSource, ToolResultContent};
use rust_cli_kernel_api::tool::{ToolError, ToolOutput, ToolUseContext, Validation};
use serde_json::json;

use crate::common::{env_failure, failed, format_bytes, missing_file, stat};
use crate::read_support::image_type;
use crate::web_fetch::SideModel;
use crate::{base64, texts};

/// 一次最多取这么多页。
const MAX_PAGES: u64 = 20;
/// 整读最多这么多页。
const WHOLE_PAGES: u64 = 10;
/// 整读的大小上限。
const WHOLE_BYTES: u64 = 20 * 1024 * 1024;
/// 取页与问页数的大小上限。
const EXTRACT_BYTES: u64 = 100 * 1024 * 1024;

/// 模型不能整读 PDF 时的说明，不点名模型（修复线 F51，tools-files.md §1.5）。
const UNSUPPORTED_MODEL: &str = "Reading full PDFs is not supported with this model. Use a model that supports PDF input, or use the pages parameter to read specific page ranges (e.g., pages: \"1-5\", maximum 20 pages per request). Page extraction requires poppler-utils: install with `brew install poppler` on macOS or `apt-get install poppler-utils` on Debian/Ubuntu.";
const PDFTOPPM_MISSING: &str = "pdftoppm is not installed. Install poppler-utils (e.g. `brew install poppler` or `apt-get install poppler-utils`) to enable PDF page rendering.";

/// 页码范围；`last` 为 None 时到文档结尾（`3-`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PageRange {
    pub(crate) first: u64,
    pub(crate) last: Option<u64>,
}

/// 开头的 ASCII 数字有几位（JS 不带 `u` 的 `\d`）。
fn digits(text: &str) -> usize {
    text.len() - text.trim_start_matches(|c: char| c.is_ascii_digit()).len()
}

/// 页码：trim 后匹配 `^(\d{1,9})(?:\s*(-)\s*(\d{1,9})?)?$`，起始页 ≥ 1，结束页 ≥ 起始页。
fn parse(pages: &str) -> Option<PageRange> {
    let text = rust_cli_base::js::string::trim(pages);
    let head = digits(text);
    if head == 0 || head > 9 {
        return None;
    }
    let first: u64 = text[..head].parse().ok()?;
    let rest = &text[head..];
    let last = if rest.is_empty() {
        Some(first)
    } else {
        let after = rest.trim_start_matches(is_space).strip_prefix('-')?;
        let after = after.trim_start_matches(is_space);
        let tail = digits(after);
        match after.len() {
            0 => None,
            _ if tail == after.len() && tail <= 9 => Some(after.parse().ok()?),
            _ => return None,
        }
    };
    (first >= 1 && last.is_none_or(|last| last >= first)).then_some(PageRange { first, last })
}

/// 页码的检查：不合法时 errorCode 7，多于 20 页（到结尾的写法按 21 页算）时 errorCode 8。
pub(crate) fn check(pages: &str) -> Result<PageRange, Validation> {
    let invalid = |message: String, code: i32| Validation::Invalid {
        message,
        error_code: Some(code),
    };
    let Some(range) = parse(pages) else {
        return Err(invalid(
            format!(
                "Invalid pages parameter: \"{pages}\". Give one page (\"3\") or one range (\"1-5\"), not a list. To read several pages or ranges, read each one separately. Pages are 1-indexed."
            ),
            7,
        ));
    };
    let count = range
        .last
        .map_or(MAX_PAGES + 1, |last| last - range.first + 1);
    if count > MAX_PAGES {
        return Err(invalid(
            format!(
                "Page range \"{pages}\" exceeds maximum of {MAX_PAGES} pages per request. Please use a smaller range."
            ),
            8,
        ));
    }
    Ok(range)
}

/// 工具被取消时不再等执行环境（丢下 future，子进程随之结束）。
async fn cancellable<T>(
    context: &ToolUseContext,
    work: impl Future<Output = T>,
) -> Result<T, ToolError> {
    tokio::select! {
        output = work => Ok(output),
        () = context.cancel.cancelled() => Err(ToolError::Cancelled),
    }
}

/// 当前模型能否整读 PDF：会话当前的主模型（取不到时用组装时的默认选择）经模型端口查生效配置的
/// `supports_pdf`（修复线 F23）。没有模型端口或答不出时按能处理。
async fn whole_allowed(context: &ToolUseContext, model: Option<&SideModel>) -> bool {
    let Some(side) = model else {
        return true;
    };
    let selection = context
        .interactions
        .current_selection(&context.session)
        .await
        .unwrap_or_else(|| side.selection.clone());
    side.model
        .capabilities(&selection)
        .await
        .is_none_or(|found| found.supports_pdf)
}

/// PDF 分支：`path` 是解析后的绝对路径，`cwd` 用于不存在时的报错。
pub(crate) async fn read(
    context: &ToolUseContext,
    (cwd, path): (&str, &str),
    pages: Option<&str>,
    model: Option<&SideModel>,
) -> Result<ToolOutput, ToolError> {
    let Some(found) = stat(context, path).await? else {
        return Err(failed(missing_file(context, cwd, path).await));
    };
    match pages {
        Some(pages) => extract(context, path, &found, pages).await,
        None => whole(context, path, &found, model).await,
    }
}

fn empty(path: &str) -> ToolError {
    failed(format!("PDF file is empty: {path}"))
}

/// 整读：页数（pdfinfo）→ 模型能力 → 大小与页数的检查 → 说明一行与 document 块。
async fn whole(
    context: &ToolUseContext,
    path: &str,
    found: &FileStat,
    model: Option<&SideModel>,
) -> Result<ToolOutput, ToolError> {
    if found.kind == FileKind::Directory {
        return Err(failed(texts::is_directory(path)));
    }
    if found.size <= EXTRACT_BYTES {
        let request = PdfPageCount {
            path: EnvPath::new(path),
        };
        let asked = context.env.pdf_page_count(&context.workspace, request);
        // 取不到页数（没装 pdfinfo、执行环境没有这项操作）时往下走。
        let count = cancellable(context, asked).await?.ok().flatten();
        if let Some(count) = count.filter(|count| *count > WHOLE_PAGES) {
            return Err(failed(format!(
                "This PDF has {count} pages, which is too many to read at once. Use the pages parameter to read specific page ranges (e.g., pages: \"1-5\"). Maximum {MAX_PAGES} pages per request."
            )));
        }
    }
    if !whole_allowed(context, model).await {
        return Err(failed(UNSUPPORTED_MODEL.to_owned()));
    }
    if found.size == 0 {
        return Err(empty(path));
    }
    if found.size > WHOLE_BYTES {
        return Err(failed(format!(
            "PDF file exceeds maximum allowed size of {}.",
            format_bytes(WHOLE_BYTES)
        )));
    }
    let request = ReadFile {
        path: EnvPath::new(path),
    };
    let file = context
        .env
        .read_file(&context.workspace, request)
        .await
        .map_err(env_failure)?;
    if file.bytes.is_empty() {
        return Err(empty(path));
    }
    if !file.bytes.starts_with(b"%PDF-") {
        return Err(failed(format!(
            "File is not a valid PDF (missing %PDF- header): {path}"
        )));
    }
    let size = file.bytes.len() as u64;
    Ok(ToolOutput {
        content: vec![
            ToolResultContent::Text {
                text: format!("PDF file read: {path} ({})", format_bytes(size)),
            },
            ToolResultContent::Document {
                source: ImageSource::Base64 {
                    media_type: "application/pdf".to_owned(),
                    data: base64::encode(&file.bytes),
                },
            },
        ],
        is_error: false,
        display: json!({"type": "pdf", "file": {"filePath": path, "originalSize": size}}),
    })
}

/// 取页：大小的检查 → 执行环境的 pdftoppm → 说明一行与每页一个 image 块。
async fn extract(
    context: &ToolUseContext,
    path: &str,
    found: &FileStat,
    pages: &str,
) -> Result<ToolOutput, ToolError> {
    let range = check(pages).map_err(|refused| match refused {
        Validation::Invalid { message, .. } => failed(message),
        Validation::Ok => failed(String::new()),
    })?;
    if found.kind != FileKind::File {
        return Err(failed(format!("Path is not a regular file: {path}")));
    }
    if found.size == 0 {
        return Err(empty(path));
    }
    if found.size > EXTRACT_BYTES {
        return Err(failed(format!(
            "PDF file exceeds maximum allowed size for text extraction ({}).",
            format_bytes(EXTRACT_BYTES)
        )));
    }
    let request = RenderPdfPages {
        path: EnvPath::new(path),
        first_page: range.first,
        last_page: range.last,
    };
    let asked = context.env.render_pdf_pages(&context.workspace, request);
    let rendered = match cancellable(context, asked).await? {
        Ok(rendered) => rendered,
        // 执行环境没有这项操作时与没装 pdftoppm 同一句（三平台、本地与远端都一样）。
        Err(EnvError::Unsupported { .. }) => PdfRender::Unavailable,
        Err(error) => return Err(env_failure(error)),
    };
    let pages = match rendered {
        PdfRender::Unavailable => return Err(failed(PDFTOPPM_MISSING.to_owned())),
        PdfRender::Failed { stderr, .. } => return Err(failed(render_failure(&stderr, range))),
        PdfRender::Rendered { pages } if pages.is_empty() => {
            return Err(failed(
                "pdftoppm produced no output pages. The PDF may be invalid.".to_owned(),
            ));
        }
        PdfRender::Rendered { pages } => pages,
    };
    let mut content = vec![ToolResultContent::Text {
        text: format!(
            "PDF pages extracted: {} page(s) from {path} ({})",
            pages.len(),
            format_bytes(found.size)
        ),
    }];
    // 页面原样内联，不缩放、不压缩（tools-files.md D4）。
    content.extend(pages.iter().map(|page| ToolResultContent::Image {
        source: ImageSource::Base64 {
            media_type: image_type(page).unwrap_or("image/jpeg").to_owned(),
            data: base64::encode(page),
        },
    }));
    Ok(ToolOutput {
        content,
        is_error: false,
        display: json!({"type": "parts", "file": {
            "filePath": path, "originalSize": found.size, "count": pages.len(),
        }}),
    })
}

/// 报错里的范围。
fn range_text(range: PageRange) -> String {
    match range.last {
        None => format!("pages {}-", range.first),
        Some(last) if last == range.first => format!("page {last}"),
        Some(last) => format!("pages {}-{last}", range.first),
    }
}

/// `prefix` 之后可以有 ` (数字)`，再是 `: `（`(?: \(\d+\))?: `）；返回其后的部分。
fn after_tag<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = text.strip_prefix(prefix)?;
    let rest = match rest.strip_prefix(" (") {
        Some(inner) if digits(inner) > 0 && inner[digits(inner)..].starts_with(')') => {
            &inner[digits(inner) + 1..]
        }
        _ => rest,
    };
    rest.strip_prefix(": ")
}

/// `/Wrong page range given.*last page \((\d+)\)/i`：同一行里，取最右边的 `last page (N)`（`.*` 贪婪）。
fn last_page(lowered: &str) -> Option<u64> {
    lowered.split('\n').find_map(|line| {
        let at = line.find("wrong page range given")?;
        let rest = &line[at..];
        rest.rmatch_indices("last page (")
            .map(|(index, found)| &rest[index + found.len()..])
            .find_map(|tail| {
                let count = digits(tail);
                (count > 0 && tail[count..].starts_with(')')).then(|| tail[..count].parse().ok())?
            })
    })
}

/// `/Syntax Error(?: \(\d+\))?: Couldn't (?:find trailer dictionary|read xref table)/i`。
fn broken_xref(lowered: &str) -> bool {
    lowered.match_indices("syntax error").any(|(index, _)| {
        after_tag(&lowered[index..], "syntax error").is_some_and(|rest| {
            rest.starts_with("couldn't find trailer dictionary")
                || rest.starts_with("couldn't read xref table")
        })
    })
}

/// pdftoppm 非零退出时给模型的文字（tools-files.md §1.5 的表）。stdin 交文件内容，stderr 里没有
/// 路径，`I/O Error: Couldn't open file '<路径>'` 一支走不到。
pub(crate) fn render_failure(stderr: &str, range: PageRange) -> String {
    let lines: Vec<&str> = stderr
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    let unknown = || format!("pdftoppm failed: {stderr}");
    let write_failed = lines.iter().any(|line| {
        line.strip_prefix("Could not write image to ")
            .is_some_and(|rest| rest.trim_end_matches(is_space).ends_with("; exiting"))
    });
    if write_failed {
        return unknown();
    }
    let lowered = lines.join("\n").to_ascii_lowercase();
    if lowered.contains("password") {
        return "PDF is password-protected. Please provide an unprotected version.".to_owned();
    }
    if let Some(count) = last_page(&lowered) {
        if count == 0 {
            return "PDF reports 0 pages (empty page tree). The PDF may be invalid.".to_owned();
        }
        let unit = if count == 1 { "page" } else { "pages" };
        return format!(
            "Requested {} is outside the document (PDF has {count} {unit}). Use a range within 1-{count}, maximum {MAX_PAGES} pages per request (e.g. pages: \"1-{}\").",
            range_text(range),
            count.min(MAX_PAGES)
        );
    }
    if ["damaged", "corrupt", "invalid"]
        .iter()
        .any(|word| lowered.contains(word))
        || broken_xref(&lowered)
    {
        return "PDF file is corrupted or invalid.".to_owned();
    }
    let first = lines.first().copied().unwrap_or_default();
    let tagged = lines.iter().any(|line| {
        after_tag(line, "Command Line Error").is_some()
            || after_tag(line, "Internal Error").is_some()
    });
    if !tagged && first.starts_with("Permission Error: ") {
        return format!("Could not render PDF: {first}");
    }
    unknown()
}

#[cfg(test)]
#[path = "read_pdf_texts_tests.rs"]
mod tests;
