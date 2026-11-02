// 复制自 apps/zcode-cli-rust/crates/bash/src/printf.rs（07ab266d4b）；不 panic，失败时按不放行处理。
//! Node `isSafePrintfArgv`: formats that only print, never assign or execute.
use regex::Regex;
use std::sync::OnceLock;

struct Patterns {
    octal_or_hex_escape: Regex,
    numeric: Regex,
    star: Regex,
    number: Regex,
}

/// 正则都是常量；万一构造失败返回 None，`safe` 按不安全处理（不 panic）。
fn patterns() -> Option<&'static Patterns> {
    static PATTERNS: OnceLock<Option<Patterns>> = OnceLock::new();
    PATTERNS
        .get_or_init(|| {
            Some(Patterns {
                octal_or_hex_escape: Regex::new(r"%[^%a-zA-Z]*(?:hh|ll|[lLhqjzZt])?\\[0-7xX]")
                    .ok()?,
                numeric: Regex::new(r"%[-+ 0#']*[0-9.*]*(?:hh|ll|[lLhqjzZt])?[diouxXeEfFgGaAn]")
                    .ok()?,
                star: Regex::new(r"%[^%a-zA-Z]*\*").ok()?,
                number: Regex::new(
                    r"^[-+]?(0[xX][0-9a-fA-F]+|[0-9]+#[0-9a-zA-Z]+|[0-9]*\.?[0-9]+([eE][-+]?[0-9]+)?)$",
                )
                .ok()?,
            })
        })
        .as_ref()
}

pub(crate) fn safe(argv: &[String]) -> bool {
    let first = argv.get(1).map(String::as_str);
    if first.is_some_and(|f| f.starts_with('-') && f != "--") {
        return false;
    }
    let mut format_index = 1;
    if first == Some("--") {
        format_index += 1;
    }
    let format = argv.get(format_index).map_or("", String::as_str);
    if format.contains('$') {
        return false;
    }
    let normalized = format.replace("%%", "");
    let Some(p) = patterns() else {
        return false;
    };
    if p.octal_or_hex_escape.is_match(&normalized)
        || normalized.contains("\\u")
        || normalized.contains("\\U")
    {
        return false;
    }
    if p.numeric.is_match(&normalized) || p.star.is_match(&normalized) {
        // 数值格式会对参数做算术求值，参数必须是纯数字字面量。
        return argv.iter().skip(format_index + 1).all(|value| {
            !(value.contains('[') || value.contains('`') || value.contains("$("))
                && p.number.is_match(value)
        });
    }
    true
}
