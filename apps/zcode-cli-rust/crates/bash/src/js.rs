//! ECMAScript string semantics the Node policy relies on.

/// JS `\s` and `String.prototype.trim` whitespace (Rust's `char::is_whitespace`
/// lacks U+FEFF and includes U+0085, so it cannot be used).
pub(crate) fn is_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

pub(crate) fn trim(s: &str) -> &str {
    s.trim_matches(is_space)
}

pub(crate) fn has_space(s: &str) -> bool {
    s.contains(is_space)
}

pub(crate) fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// `/^\d+$/` (JS `\d` is ASCII only).
pub(crate) fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// `/^-\d+$/`.
pub(crate) fn is_dash_digits(s: &str) -> bool {
    s.strip_prefix('-').is_some_and(is_digits)
}

const SPACE_CLASS: &str = r"\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}";

/// Compiles an ECMAScript regex source without flags. `\s`, `\S`, `\d`, `\D`,
/// `\w` and `\W` are rewritten to their ASCII / JS meanings; every other escape
/// used by the exported tables means the same in the `regex` crate.
pub(crate) fn regex(source: &str) -> regex::Regex {
    let mut out = String::with_capacity(source.len() + 16);
    let mut chars = source.chars().peekable();
    let mut in_class = false;
    while let Some(c) = chars.next() {
        if c == '\\' {
            let Some(next) = chars.next() else {
                out.push('\\');
                break;
            };
            let class = match next {
                's' => Some((SPACE_CLASS, false)),
                'S' => Some((SPACE_CLASS, true)),
                'd' => Some(("0-9", false)),
                'D' => Some(("0-9", true)),
                'w' => Some(("A-Za-z0-9_", false)),
                'W' => Some(("A-Za-z0-9_", true)),
                _ => None,
            };
            match class {
                Some((body, negated)) if in_class => {
                    assert!(!negated, "negated escape inside a class: {source}");
                    out.push_str(body);
                }
                Some((body, negated)) => {
                    out.push('[');
                    if negated {
                        out.push('^');
                    }
                    out.push_str(body);
                    out.push(']');
                }
                None => {
                    out.push('\\');
                    out.push(next);
                }
            }
            continue;
        }
        match c {
            '[' if !in_class => in_class = true,
            ']' if in_class => in_class = false,
            _ => {}
        }
        out.push(c);
    }
    regex::Regex::new(&out).unwrap_or_else(|e| panic!("unsupported regex {source}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_semantics() {
        assert_eq!(trim("\u{FEFF} a \u{2028}"), "a");
        assert_eq!(trim("\u{85}a"), "\u{85}a");
        assert!(!is_digits("١٢"));
        let re = regex(r"^hostname(?:\s+(?:-[a-zA-Z]|--[a-zA-Z-]+))*\s*$");
        assert!(re.is_match("hostname -f\u{FEFF}"));
        assert!(!re.is_match("hostname\u{85}-f"));
    }
}
