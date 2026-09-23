//! Node `isSafePrintfArgv`: formats that only print, never assign or execute.
use regex::Regex;
use std::sync::OnceLock;

struct Patterns {
    octal_or_hex_escape: Regex,
    numeric: Regex,
    star: Regex,
    number: Regex,
}

fn patterns() -> &'static Patterns {
    static PATTERNS: OnceLock<Patterns> = OnceLock::new();
    PATTERNS.get_or_init(|| Patterns {
        octal_or_hex_escape: Regex::new(r"%[^%a-zA-Z]*(?:hh|ll|[lLhqjzZt])?\\[0-7xX]").unwrap(),
        numeric: Regex::new(r"%[-+ 0#']*[0-9.*]*(?:hh|ll|[lLhqjzZt])?[diouxXeEfFgGaAn]").unwrap(),
        star: Regex::new(r"%[^%a-zA-Z]*\*").unwrap(),
        number: Regex::new(
            r"^[-+]?(0[xX][0-9a-fA-F]+|[0-9]+#[0-9a-zA-Z]+|[0-9]*\.?[0-9]+([eE][-+]?[0-9]+)?)$",
        )
        .unwrap(),
    })
}

pub(crate) fn safe(argv: &[String]) -> bool {
    let first = argv.get(1).map(String::as_str);
    if first.is_some_and(|f| f.starts_with('-') && f != "--") {
        return false;
    }
    let format_index = if first == Some("--") { 2 } else { 1 };
    let format = argv.get(format_index).map_or("", String::as_str);
    if format.contains('$') {
        return false;
    }
    let normalized = format.replace("%%", "");
    let p = patterns();
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
