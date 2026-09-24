//! Read of PDF files without IO (Node `read-pdf.ts`, `contracts/src/tools/read-pdf.ts`
//! and the Poppler adapter's diagnostics). Spec rust-m5-tools §6.

pub const NATIVE_MAX_BYTES: u64 = 20 * 1024 * 1024;
pub const EXTRACT_MAX_BYTES: u64 = 100 * 1024 * 1024;
pub const NATIVE_MAX_PAGES: u64 = 10;
pub const MAX_PAGES: u64 = 20;
pub const INFO_TIMEOUT_MS: u64 = 10_000;
pub const PROBE_TIMEOUT_MS: u64 = 5_000;
pub const RENDER_TIMEOUT_MS: u64 = 120_000;
pub const PAGES_DESCRIPTION: &str = "- Reads PDFs via the `pages` parameter (e.g. \"1-5\", max 20 pages/request; required for PDFs over 10 pages).";

/// Node `ReadErrorCode`.
pub mod code {
    pub const IMAGES_UNSUPPORTED: u32 = 10;
    pub const CONFIGURATION: u32 = 11;
    pub const INVALID: u32 = 12;
    pub const TOO_LARGE: u32 = 13;
    pub const TOO_MANY_PAGES: u32 = 14;
    pub const TIMEOUT: u32 = 15;
    pub const PASSWORD: u32 = 16;
    pub const OUT_OF_RANGE: u32 = 17;
    pub const PERMISSION: u32 = 18;
    pub const IO: u32 = 19;
    pub const PROCESS: u32 = 20;
}

/// `first..=last`; `last: None` is Node's open range `N-`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub first: u64,
    pub last: Option<u64>,
}

const MAX_SAFE: u64 = 9_007_199_254_740_991;

fn number(digits: &str) -> Option<u64> {
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u64>().ok().filter(|n| *n <= MAX_SAFE)
}

/// Node `parseReadPdfPageRange`.
pub fn parse_range(value: &str) -> Option<Range> {
    let value = crate::js_string::trim(value);
    if let Some(first) = value.strip_suffix('-') {
        return number(first)
            .filter(|n| *n >= 1)
            .map(|first| Range { first, last: None });
    }
    let (first, last) = match value.split_once('-') {
        Some((first, last)) => (number(first)?, number(last)?),
        None => {
            let page = number(value)?;
            (page, page)
        }
    };
    (first >= 1 && last >= first).then_some(Range {
        first,
        last: Some(last),
    })
}

/// Node `getReadPdfPagesValidationFailure`: the model-visible message.
pub fn pages_error(pages: &str) -> Option<String> {
    let Some(range) = parse_range(pages) else {
        return Some(format!(
            "Invalid pages parameter: \"{pages}\". Use formats like \"1-5\", \"3\", or \"10-20\". Pages are 1-indexed."
        ));
    };
    let count = range
        .last
        .map_or(MAX_PAGES + 1, |last| last - range.first + 1);
    (count > MAX_PAGES).then(|| {
        format!(
            "Page range \"{pages}\" exceeds maximum of {MAX_PAGES} pages per request. Please use a smaller range."
        )
    })
}

/// Node `formatFileSize` (read-pdf): `N bytes`, then one decimal without `.0`.
pub fn file_size(bytes: u64) -> String {
    let one = |value: f64| {
        let text = format!("{value:.1}");
        text.strip_suffix(".0").map(str::to_owned).unwrap_or(text)
    };
    match bytes {
        b if b < 1024 => format!("{b} bytes"),
        b if b < 1 << 20 => format!("{}KB", one(b as f64 / 1024.0)),
        b if b < 1 << 30 => format!("{}MB", one(b as f64 / (1u64 << 20) as f64)),
        b => format!("{}GB", one(b as f64 / (1u64 << 30) as f64)),
    }
}

/// `pdfinfo`'s `Pages:` line.
pub fn page_count(stdout: &str) -> Option<u64> {
    stdout.lines().find_map(|line| {
        let rest = line.strip_prefix("Pages:")?;
        number(rest.trim())
    })
}

/// Node `renderedPageNumber`: `…-<n>.jpg`.
pub fn page_number(name: &str) -> Option<u64> {
    let lower = name.to_lowercase();
    let stem = lower.strip_suffix(".jpg")?;
    let (_, digits) = stem.rsplit_once('-')?;
    number(digits).filter(|n| *n >= 1)
}

/// Node `assertRenderSucceeded` for a failed `pdftoppm`: `(code, message)`.
pub fn render_failure(
    (stderr, stdout): (&str, &str),
    path: &str,
    (first, last): (u64, u64),
) -> (u32, String) {
    let lower = stderr.to_lowercase();
    if lower.contains("password") {
        return (
            code::PASSWORD,
            "PDF is password-protected. Please provide an unprotected version.".into(),
        );
    }
    if let Some(at) = lower.find("wrong page range given") {
        let rest = &stderr[at..];
        let count = rest.to_lowercase().find("last page (").and_then(|i| {
            rest[i + "last page (".len()..]
                .split(')')
                .next()
                .and_then(number)
        });
        if count == Some(0) {
            return (
                code::INVALID,
                "PDF reports 0 pages (empty page tree). The PDF may be invalid.".into(),
            );
        }
        if let Some(count) = count {
            let requested = if first == last {
                format!("page {first}")
            } else {
                format!("pages {first}-{last}")
            };
            let plural = if count == 1 { "" } else { "s" };
            let end = count.min(MAX_PAGES);
            return (
                code::OUT_OF_RANGE,
                format!(
                    "Requested {requested} is outside the document (PDF has {count} page{plural}). Use a range within 1-{count}, maximum {MAX_PAGES} pages per request (e.g. pages: \"1-{end}\")."
                ),
            );
        }
    }
    let lines: Vec<&str> = stderr.split('\n').collect();
    let first_line = lines.first().copied().unwrap_or("");
    let internal = lines.iter().any(|line| {
        ["Command Line Error", "Internal Error"]
            .iter()
            .any(|prefix| {
                line.strip_prefix(prefix).is_some_and(|rest| {
                    let rest = rest
                        .strip_prefix(' ')
                        .and_then(|r| r.strip_prefix('('))
                        .map_or(rest, |r| r.split_once(')').map_or(r, |(_, after)| after));
                    rest.starts_with(": ")
                })
            })
    });
    let io = first_line.starts_with("I/O Error: ") && first_line.contains(&format!("'{path}'"));
    let permission = first_line.starts_with("Permission Error: ");
    if (io || permission) && !internal {
        let code = if permission {
            code::PERMISSION
        } else {
            code::IO
        };
        return (code, format!("Could not render PDF: {first_line}"));
    }
    let broken = lower.contains("couldn't find trailer dictionary")
        || lower.contains("couldn't read xref table");
    if ["damaged", "corrupt", "invalid"]
        .iter()
        .any(|w| lower.contains(w))
        || broken
    {
        return (code::INVALID, "PDF file is corrupted or invalid.".into());
    }
    let detail = [stderr, stdout]
        .iter()
        .filter(|v| !v.trim().is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    let detail = detail.trim();
    let message = if detail.is_empty() {
        "pdftoppm failed.".to_owned()
    } else {
        format!("pdftoppm failed: {detail}")
    };
    (code::PROCESS, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_and_texts_follow_node() {
        assert_eq!(
            parse_range(" 3 "),
            Some(Range {
                first: 3,
                last: Some(3)
            })
        );
        assert_eq!(
            parse_range("2-5"),
            Some(Range {
                first: 2,
                last: Some(5)
            })
        );
        assert_eq!(
            parse_range("4-"),
            Some(Range {
                first: 4,
                last: None
            })
        );
        for bad in ["0", "5-2", "a", "1-2-3", "-3", ""] {
            assert_eq!(parse_range(bad), None, "{bad}");
        }
        assert_eq!(
            pages_error("x").unwrap(),
            "Invalid pages parameter: \"x\". Use formats like \"1-5\", \"3\", or \"10-20\". Pages are 1-indexed."
        );
        assert_eq!(
            pages_error("1-21").unwrap(),
            "Page range \"1-21\" exceeds maximum of 20 pages per request. Please use a smaller range."
        );
        assert!(pages_error("3-").is_some());
        assert_eq!(pages_error("1-20"), None);
        assert_eq!(file_size(900), "900 bytes");
        assert_eq!(file_size(1536), "1.5KB");
        assert_eq!(file_size(20 << 20), "20MB");
        assert_eq!(page_count("Title: x\nPages:          12\n"), Some(12));
        assert_eq!(page_number("page-07.jpg"), Some(7));
        assert_eq!(page_number("page-0.jpg"), None);
        assert_eq!(page_number("page-3.png"), None);
    }

    #[test]
    fn poppler_diagnostics_are_classified_like_node() {
        let path = "/w/a.pdf";
        assert_eq!(
            render_failure(("Command Line Error: Incorrect password", ""), path, (1, 1)).0,
            code::PASSWORD
        );
        let (code_, message) = render_failure(
            (
                "Wrong page range given: the first page (5) can not be after the last page (3).",
                "",
            ),
            path,
            (5, 6),
        );
        assert_eq!(code_, code::OUT_OF_RANGE);
        assert_eq!(
            message,
            "Requested pages 5-6 is outside the document (PDF has 3 pages). Use a range within 1-3, maximum 20 pages per request (e.g. pages: \"1-3\")."
        );
        assert_eq!(
            render_failure(
                ("I/O Error: Couldn't open file '/w/a.pdf': No such file", ""),
                path,
                (1, 1)
            ),
            (
                code::IO,
                "Could not render PDF: I/O Error: Couldn't open file '/w/a.pdf': No such file"
                    .into()
            )
        );
        assert_eq!(
            render_failure(("Syntax Error: Couldn't read xref table", ""), path, (1, 1)).1,
            "PDF file is corrupted or invalid."
        );
        assert_eq!(render_failure(("", ""), path, (1, 1)).1, "pdftoppm failed.");
        assert_eq!(
            render_failure(("boom", "out"), path, (1, 1)).1,
            "pdftoppm failed: boom\nout"
        );
    }
}
