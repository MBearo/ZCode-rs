//! JavaScript string rules shared by ports of Node text logic: the `\s` /
//! `String.prototype.trim` whitespace set and UTF-16 lengths.

/// JS `\s` and `trim` whitespace: includes U+FEFF, excludes U+0085.
pub fn is_space(c: char) -> bool {
    matches!(
        c,
        '\u{9}'..='\u{d}'
            | ' '
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

/// JS `String.prototype.trim`.
pub fn trim(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// JS `s.slice(0, units)` without splitting a character.
pub fn utf16_prefix(s: &str, units: usize) -> &str {
    let mut used = 0;
    for (at, c) in s.char_indices() {
        used += c.len_utf16();
        if used > units {
            return &s[..at];
        }
    }
    s
}

const MESSAGE_LIMIT: usize = 500;
const MESSAGE_KEEP: usize = 497;

/// Node `sanitizeText` of error payloads: whitespace runs collapse to one
/// space, the result is trimmed and capped at 500 UTF-16 units; `None` when empty.
pub fn sanitize_message(message: &str) -> Option<String> {
    let compact = message
        .split(is_space)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if compact.is_empty() {
        return None;
    }
    if compact.encode_utf16().count() <= MESSAGE_LIMIT {
        return Some(compact);
    }
    Some(format!("{}...", utf16_prefix(&compact, MESSAGE_KEEP)))
}

/// ECMAScript `GetSubstitution`: `String.prototype.replace` with a string
/// replacement expands `$$`, `$&`, `` $` ``, `$'` and `$n` / `$nn` (unnamed
/// groups only). `captures[i]` is group `i + 1`.
pub fn substitute(
    replacement: &str,
    (before, matched, after): (&str, &str, &str),
    captures: &[Option<&str>],
) -> String {
    let mut out = String::with_capacity(replacement.len());
    let bytes = replacement.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' || i + 1 >= bytes.len() {
            let ch = replacement[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        let digit = |at: usize| {
            bytes
                .get(at)
                .filter(|b| b.is_ascii_digit())
                .map(|b| (b - b'0') as usize)
        };
        match bytes[i + 1] {
            b'$' => out.push('$'),
            b'&' => out.push_str(matched),
            b'`' => out.push_str(before),
            b'\'' => out.push_str(after),
            _ => {
                let group = match (digit(i + 1), digit(i + 2)) {
                    (Some(a), Some(b)) if (1..=captures.len()).contains(&(a * 10 + b)) => {
                        Some((a * 10 + b, 3))
                    }
                    (Some(a), _) if (1..=captures.len()).contains(&a) => Some((a, 2)),
                    _ => None,
                };
                match group {
                    Some((n, len)) => {
                        out.push_str(captures[n - 1].unwrap_or(""));
                        i += len;
                    }
                    None => {
                        out.push('$');
                        i += 1;
                    }
                }
                continue;
            }
        }
        i += 2;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_like_node() {
        assert_eq!(sanitize_message("  a \n\t b\u{feff}"), Some("a b".into()));
        assert_eq!(sanitize_message(" \n "), None);
        let long = "字".repeat(600);
        let cut = sanitize_message(&long).unwrap();
        assert_eq!(cut.chars().count(), 500);
        assert!(cut.ends_with("..."));
        assert_eq!(utf16_prefix("a😀b", 2), "a");
        assert_eq!(trim("\u{85}x\u{feff}"), "\u{85}x");
    }
}
