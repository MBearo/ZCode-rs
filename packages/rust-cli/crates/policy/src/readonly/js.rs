// 复制自 apps/zcode-cli-rust/crates/bash/src/js.rs（07ab266d4b）；不 panic，失败时按不放行处理。
//! ECMAScript string semantics the Node policy relies on.

/// JS `\s` 与 `String.prototype.trim` 的空白（Rust 的 `char::is_whitespace` 没有 U+FEFF、多了 U+0085，不能用）
/// 与 JS 的字符串长度：唯一定义在 base（F58：以前这里与 `parse/js.rs` 各写一份）。
pub(crate) use rust_cli_base::js::string::{is_space, utf16_len};

/// `/^\d+$/` (JS `\d` is ASCII only).
pub(crate) fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// `/^-\d+$/`.
pub(crate) fn is_dash_digits(s: &str) -> bool {
    s.strip_prefix('-').is_some_and(is_digits)
}

/// 改写转义后正则变长的预留字节。
const CAPACITY_HEADROOM: usize = 16;

const SPACE_CLASS: &str = r"\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}";

/// Compiles an ECMAScript regex source without flags. `\s`, `\S`, `\d`, `\D`,
/// `\w` and `\W` are rewritten to their ASCII / JS meanings; every other escape
/// used by the exported tables means the same in the `regex` crate.
pub(crate) fn regex(source: &str) -> Option<regex::Regex> {
    let mut out = String::with_capacity(source.len() + CAPACITY_HEADROOM);
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
    // 表里的正则都能转换；万一不能，调用方按“不放行”处理（不 panic）。
    regex::Regex::new(&out).ok()
}

#[cfg(test)]
mod tests {
    use rust_cli_base::js::string::trim;

    use super::*;

    #[test]
    fn js_semantics() {
        assert_eq!(trim("\u{FEFF} a \u{2028}"), "a");
        assert_eq!(trim("\u{85}a"), "\u{85}a");
        assert!(!is_digits("١٢"));
        let re = regex(r"^hostname(?:\s+(?:-[a-zA-Z]|--[a-zA-Z-]+))*\s*$").unwrap();
        assert!(re.is_match("hostname -f\u{FEFF}"));
        assert!(!re.is_match("hostname\u{85}-f"));
    }
}
