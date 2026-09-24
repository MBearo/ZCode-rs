//! Node `webfetch-content.ts`: text-like responses as the processing model
//! reads them, HTML through Node's regex pipeline (JS regex semantics:
//! ASCII `\b` and ASCII-only case folding).
use super::{MAX_MODEL_INPUT_CHARS, WebError};
use crate::js_string;
use regex::Regex;
use std::sync::OnceLock;

/// JS `\s` (Rust's `\s` has U+0085 and lacks U+FEFF).
const JS_SPACE: &str = r"[\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}]";

struct Rules {
    removed: Vec<Regex>,
    replaced: Vec<(Regex, &'static str)>,
    spaces: Regex,
}

fn rules() -> &'static Rules {
    static RULES: OnceLock<Rules> = OnceLock::new();
    RULES.get_or_init(|| {
        let element =
            |tag: &str, body: &str| format!(r"(?i-u:<{tag})(?-u:\b){body}(?i-u:</{tag}>)");
        let mut removed = vec![Regex::new(r"<!--(?s:.)*?-->").unwrap()];
        for tag in ["script", "style", "noscript"] {
            removed.push(Regex::new(&element(tag, "(?s:.)*?")).unwrap());
        }
        let mut replaced: Vec<(Regex, &'static str)> = HEADINGS
            .iter()
            .enumerate()
            .map(|(i, replacement)| {
                let pattern = element(&format!("h{}", i + 1), "[^>]*>((?s:.)*?)");
                (Regex::new(&pattern).unwrap(), *replacement)
            })
            .collect();
        let anchor =
            r#"(?i-u:<a)(?-u:\b)[^>]*(?i-u:href=)["']([^"']+)["'][^>]*>((?s:.)*?)(?i-u:</a>)"#;
        replaced.push((Regex::new(anchor).unwrap(), "[$2]($1)"));
        replaced.push((
            Regex::new(&element("li", "[^>]*>((?s:.)*?)")).unwrap(),
            "\n- $1",
        ));
        let br = format!(r"(?i-u:<br){JS_SPACE}*/?>");
        replaced.push((Regex::new(&br).unwrap(), "\n"));
        let closing = r"(?i-u:</(?:p|div|section|article|header|footer|tr|table|ul|ol)>)";
        replaced.push((Regex::new(closing).unwrap(), "\n"));
        replaced.push((Regex::new(r"<[^>]+>").unwrap(), ""));
        Rules {
            removed,
            replaced,
            spaces: Regex::new(r"[ \t]+").unwrap(),
        }
    })
}

const HEADINGS: [&str; 6] = [
    "\n# $1\n",
    "\n## $1\n",
    "\n### $1\n",
    "\n#### $1\n",
    "\n##### $1\n",
    "\n###### $1\n",
];

/// JS `String.fromCodePoint` range error text (`Number` formatting).
fn code_point_error(value: f64) -> WebError {
    let number = if value.is_infinite() {
        "Infinity".to_owned()
    } else if value >= 1e21 {
        let text = format!("{value:e}");
        text.replacen('e', "e+", 1)
    } else {
        format!("{value}")
    };
    WebError {
        code: None,
        message: format!("Invalid code point {number}"),
    }
}

/// Replaces `&#x…;` (hex, `x` either case) or `&#…;` (decimal) in UTF-16,
/// so separately encoded surrogate halves join as in JS.
fn numeric_entities(units: &[u16], hex: bool) -> Result<Vec<u16>, WebError> {
    let digit = |u: u16| {
        let c = char::from_u32(u32::from(u))?;
        if hex {
            c.to_digit(16)
        } else {
            c.to_digit(10).filter(|_| c.is_ascii_digit())
        }
    };
    let mut out = Vec::with_capacity(units.len());
    let mut i = 0;
    while i < units.len() {
        let prefix = units[i] == u16::from(b'&') && units.get(i + 1) == Some(&u16::from(b'#'));
        let marker = !hex
            || units
                .get(i + 2)
                .is_some_and(|u| *u == u16::from(b'x') || *u == u16::from(b'X'));
        let start = i + if hex { 3 } else { 2 };
        if prefix && marker {
            let mut end = start;
            let mut value = 0f64;
            while let Some(d) = units.get(end).and_then(|u| digit(*u)) {
                value = value * if hex { 16.0 } else { 10.0 } + f64::from(d);
                end += 1;
            }
            if end > start && units.get(end) == Some(&u16::from(b';')) {
                if value > f64::from(0x10FFFF) {
                    return Err(code_point_error(value));
                }
                let point = value as u32;
                match char::from_u32(point) {
                    Some(c) => out.extend(c.encode_utf16(&mut [0; 2]).iter()),
                    // 孤立代理项：保留码元，与后续代理项在解码时配对（JS 字符串语义）。
                    None => out.push(point as u16),
                }
                i = end + 1;
                continue;
            }
        }
        out.push(units[i]);
        i += 1;
    }
    Ok(out)
}

/// Node `decodeHtmlEntities`, in Node's order.
fn decode_entities(text: &str) -> Result<String, WebError> {
    let named = text
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    let units: Vec<u16> = named.encode_utf16().collect();
    let units = numeric_entities(&units, true)?;
    let units = numeric_entities(&units, false)?;
    // Rust 字符串不能保存孤立代理项：未配对的以 U+FFFD 代替（Node 原样保留）。
    Ok(String::from_utf16_lossy(&units))
}

/// Node `htmlToMarkdown`.
pub fn html_to_markdown(html: &str) -> Result<String, WebError> {
    let rules = rules();
    let mut content = html.to_owned();
    for pattern in &rules.removed {
        content = pattern.replace_all(&content, "").into_owned();
    }
    for (pattern, replacement) in &rules.replaced {
        content = pattern.replace_all(&content, *replacement).into_owned();
    }
    let decoded = decode_entities(&content)?;
    let lines: Vec<String> = decoded
        .split('\n')
        .map(|line| js_string::trim(&rules.spaces.replace_all(line, " ")).to_owned())
        .collect();
    let kept: Vec<&str> = lines
        .iter()
        .enumerate()
        .filter(|(i, line)| {
            !line.is_empty() || i.checked_sub(1).is_none_or(|p| !lines[p].is_empty())
        })
        .map(|(_, line)| line.as_str())
        .collect();
    Ok(js_string::trim(&kept.join("\n")).to_owned())
}

fn text_like(mime: &str) -> bool {
    mime.is_empty()
        || mime.starts_with("text/")
        || matches!(
            mime,
            "application/json"
                | "application/xml"
                | "application/xhtml+xml"
                | "application/javascript"
                | "application/x-javascript"
        )
        || mime.ends_with("+json")
        || mime.ends_with("+xml")
}

/// Node `extractReadableContent`: UTF-8 whatever the charset says (BOM
/// dropped, invalid bytes as U+FFFD); HTML becomes markdown.
pub fn extract_readable(body: &[u8], content_type: &str) -> Result<String, WebError> {
    let mime = js_string::trim(content_type.split(';').next().unwrap_or("")).to_lowercase();
    if !text_like(&mime) {
        let shown = if mime.is_empty() { "unknown" } else { &mime };
        return Err(WebError::new(
            "webfetch_fetch_failed",
            format!("Unsupported WebFetch content type: {shown}"),
        ));
    }
    let text = String::from_utf8_lossy(body);
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(&text);
    if mime == "text/html" || mime == "application/xhtml+xml" || content_type.contains("html") {
        return html_to_markdown(text);
    }
    Ok(js_string::trim(text).to_owned())
}

/// Node `truncateContentForModel` (UTF-16 lengths).
pub fn truncate_for_model(content: &str) -> (String, bool) {
    if content.encode_utf16().count() <= MAX_MODEL_INPUT_CHARS {
        return (content.to_owned(), false);
    }
    const SUFFIX: &str = "\n\n[WebFetch content truncated before prompt processing]";
    let head = js_string::utf16_prefix(content, MAX_MODEL_INPUT_CHARS - SUFFIX.len());
    (format!("{head}{SUFFIX}"), true)
}
