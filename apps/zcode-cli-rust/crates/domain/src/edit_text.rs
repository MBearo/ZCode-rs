//! JS string rules the Edit matchers rely on: regex-equivalent unescaping,
//! Read line-number prefixes and `String.prototype.trim`.

/// JS `.` excludes line terminators.
fn no_line_terminator(s: &str) -> bool {
    !s.contains(['\n', '\r', '\u{2028}', '\u{2029}'])
}

/// `^\d+: (.*)$` or `^\d+\t(.*)$` on every line.
pub(super) fn strip_line_numbers(search: &str) -> Option<String> {
    let lines: Option<Vec<&str>> = search
        .split('\n')
        .map(|line| {
            let digits = line.bytes().take_while(u8::is_ascii_digit).count();
            if digits == 0 {
                return None;
            }
            let rest = &line[digits..];
            let body = rest
                .strip_prefix(": ")
                .or_else(|| rest.strip_prefix('\t'))?;
            no_line_terminator(body).then_some(body)
        })
        .collect();
    lines.map(|lines| lines.join("\n"))
}

/// `search.replace(/\\([ntr"'`\\$])/g, …)`.
pub(super) fn unescape_visible(search: &str) -> String {
    let mut out = String::with_capacity(search.len());
    let mut chars = search.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\'
            && let Some(&next) = chars.peek()
            && matches!(next, 'n' | 't' | 'r' | '"' | '\'' | '`' | '\\' | '$')
        {
            chars.next();
            out.push(match next {
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                other => other,
            });
            continue;
        }
        out.push(c);
    }
    out
}

/// `search.replace(/(\\\\)|\\u([0-9a-fA-F]{4})/g, …)` over UTF-16 code units;
/// `None` when the result would hold an unpaired surrogate (it can never match
/// UTF-8 file content, as in Node).
pub(super) fn unescape_unicode(search: &str) -> Option<String> {
    let units: Vec<u16> = search.encode_utf16().collect();
    let mut out = Vec::with_capacity(units.len());
    let backslash = u16::from(b'\\');
    let mut i = 0;
    while i < units.len() {
        if units[i] == backslash && units.get(i + 1) == Some(&backslash) {
            out.extend_from_slice(&units[i..i + 2]);
            i += 2;
            continue;
        }
        if units[i] == backslash && units.get(i + 1) == Some(&u16::from(b'u')) {
            let hex = units
                .get(i + 2..i + 6)
                .and_then(|h| String::from_utf16(h).ok());
            if let Some(code) = hex
                .filter(|h| h.len() == 4 && h.bytes().all(|b| b.is_ascii_hexdigit()))
                .and_then(|h| u16::from_str_radix(&h, 16).ok())
            {
                out.push(code);
                i += 6;
                continue;
            }
        }
        out.push(units[i]);
        i += 1;
    }
    String::from_utf16(&out).ok()
}

pub use crate::js_string::trim as js_trim;
