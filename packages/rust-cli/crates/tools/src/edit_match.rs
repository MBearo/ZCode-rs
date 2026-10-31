//! Edit 的字符串匹配与替换（找到实际的字符串、保留引号风格、套用编辑）。下标按 UTF-16 语义处理的地方都换成
//! 按字符处理，结果相同。

use regex::Regex;

/// `\uXXXX`：前缀两个字符加四位十六进制。
const ESCAPE_PREFIX: &str = "\\u";
const ESCAPE_LEN: usize = 6;
const HEX_RADIX: u32 = rust_cli_base::units::HEX_RADIX;
/// 不到它的 UTF-16 码元是 ASCII，不转义。
const ASCII_END: u16 = 0x80;
const LEFT_SINGLE: char = '\u{2018}';
const RIGHT_SINGLE: char = '\u{2019}';
const LEFT_DOUBLE: char = '\u{201C}';
const RIGHT_DOUBLE: char = '\u{201D}';

fn straight(c: char) -> char {
    match c {
        LEFT_SINGLE | RIGHT_SINGLE => '\'',
        LEFT_DOUBLE | RIGHT_DOUBLE => '"',
        other => other,
    }
}

/// `\uXXXX` 转义。
fn has_escape(text: &str) -> bool {
    text.as_bytes().windows(ESCAPE_LEN).any(|window| {
        window[0] == b'\\' && window[1] == b'u' && window[2..].iter().all(u8::is_ascii_hexdigit)
    })
}

fn has_non_ascii(text: &str) -> bool {
    !text.is_ascii()
}

/// 没找到时是否附加转义提示。
pub(crate) fn wants_escape_hint(old: &str) -> bool {
    has_escape(old) || has_non_ascii(old)
}

/// 把 `\uXXXX` 解成字符，`\\` 原样保留。孤立的代理项无法表示，原样保留转义。
fn unescape(text: &str) -> String {
    let Ok(pattern) = Regex::new(r"(\\\\)|\\u([0-9a-fA-F]{4})") else {
        return text.to_owned();
    };
    pattern
        .replace_all(text, |captures: &regex::Captures<'_>| {
            if let Some(pair) = captures.get(1) {
                return pair.as_str().to_owned();
            }
            u32::from_str_radix(&captures[2], HEX_RADIX)
                .ok()
                .and_then(char::from_u32)
                .map_or_else(|| captures[0].to_owned(), String::from)
        })
        .into_owned()
}

/// 转义后的长度：每个非 ASCII 的 UTF-16 码元变成 6 个字符。
fn escaped_len(text: &str) -> usize {
    text.encode_utf16()
        .map(|unit| if unit >= ASCII_END { ESCAPE_LEN } else { 1 })
        .sum()
}

fn trailing_backslashes(text: &str, end: usize) -> usize {
    text.as_bytes()[..end]
        .iter()
        .rev()
        .take_while(|byte| **byte == b'\\')
        .count()
}

/// 在 `haystack` 里找 `needle` 的 `\uXXXX` 转义形式。
fn find_escaped(needle: &str, haystack: &str) -> Option<String> {
    if !trailing_backslashes(needle, needle.len()).is_multiple_of(2) {
        return None;
    }
    let mut escapes = if needle.starts_with('\\') {
        vec![0]
    } else {
        Vec::new()
    };
    let mut source = String::new();
    let mut offset = 0;
    for c in needle.chars() {
        let mut units = [0_u16; 2];
        for unit in c.encode_utf16(&mut units).iter() {
            if *unit >= ASCII_END {
                escapes.push(offset);
                offset += ESCAPE_LEN;
                source.push_str(r"\\u");
                for digit in format!("{unit:04x}").chars() {
                    if digit.is_ascii_alphabetic() {
                        source.push_str(&format!("[{digit}{}]", digit.to_ascii_uppercase()));
                    } else {
                        source.push(digit);
                    }
                }
            } else {
                offset += 1;
                source.push_str(&regex::escape(&c.to_string()));
            }
        }
    }
    let pattern = Regex::new(&source).ok()?;
    let mut start = 0;
    while let Some(found) = pattern.find_at(haystack, start) {
        let index = found.start();
        if escapes
            .iter()
            .all(|item| trailing_backslashes(haystack, index + item).is_multiple_of(2))
        {
            return (haystack.find(found.as_str()) == Some(index))
                .then(|| found.as_str().to_owned());
        }
        start = index + haystack[index..].chars().next().map_or(1, char::len_utf8);
    }
    None
}

/// 文件里与 `old` 对应的实际文本：原样、弯引号归一、`\uXXXX` 两种写法。
pub(crate) fn find_actual(content: &str, old: &str) -> Option<String> {
    if content.contains(old) {
        return Some(old.to_owned());
    }
    // 弯引号按字符一一映射成直引号，长度（字符数）不变：在归一后的文本里找，按字符位置取回原文。
    let normalized: String = content.chars().map(straight).collect();
    let target: String = old.chars().map(straight).collect();
    if let Some(byte) = normalized.find(&target) {
        let start = normalized[..byte].chars().count();
        let length = target.chars().count();
        return Some(content.chars().skip(start).take(length).collect());
    }
    if has_escape(old) {
        let decoded = unescape(old);
        if decoded != old && content.contains(&decoded) {
            return Some(decoded);
        }
    }
    if has_non_ascii(old) {
        if escaped_len(old) > content.encode_utf16().count() || !content.contains("\\u") {
            return None;
        }
        return find_escaped(old, content);
    }
    None
}

/// 引号前面是“开头”的位置。
fn opens(chars: &[char], index: usize) -> bool {
    index == 0
        || matches!(
            chars[index - 1],
            ' ' | '\t' | '\n' | '\r' | '(' | '[' | '{' | '\u{2014}' | '\u{2013}'
        )
}

fn curl_double(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    (0..chars.len())
        .map(|index| match chars[index] {
            '"' if opens(&chars, index) => LEFT_DOUBLE,
            '"' => RIGHT_DOUBLE,
            other => other,
        })
        .collect()
}

fn curl_single(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    (0..chars.len())
        .map(|index| {
            if chars[index] != '\'' {
                return chars[index];
            }
            let before = index.checked_sub(1).map(|at| chars[at]);
            let after = chars.get(index + 1).copied();
            if before.is_some_and(char::is_alphabetic) && after.is_some_and(char::is_alphabetic) {
                RIGHT_SINGLE
            } else if opens(&chars, index) {
                LEFT_SINGLE
            } else {
                RIGHT_SINGLE
            }
        })
        .collect()
}

/// 让 new_string 跟上文件里实际用的写法（保留引号风格与 `\uXXXX` 换写）。
pub(crate) fn adapt_new(old: &str, actual: &str, new: &str) -> String {
    if old == actual {
        return new.to_owned();
    }
    let mut adapted = new.to_owned();
    if actual.contains([LEFT_DOUBLE, RIGHT_DOUBLE]) {
        adapted = curl_double(&adapted);
    }
    if actual.contains([LEFT_SINGLE, RIGHT_SINGLE]) {
        adapted = curl_single(&adapted);
    }
    if has_non_ascii(old)
        && escaped_len(old) == actual.encode_utf16().count()
        && find_escaped(old, actual).is_some()
    {
        return escape_like(old, actual, &adapted);
    }
    if has_escape(old) && unescape(old) == actual {
        return unescape(&adapted);
    }
    adapted
}

/// 把 `text` 里的非 ASCII 字符写成 `\uXXXX`；`old` 里出现过的字符沿用文件里的大小写，其余按多数。
fn escape_like(old: &str, actual: &str, text: &str) -> String {
    let mut known = std::collections::HashMap::new();
    let (mut upper, mut lower) = (0, 0);
    let mut cursor = 0;
    for unit in old.encode_utf16() {
        if unit >= ASCII_END {
            let digits = actual
                .get(cursor + ESCAPE_PREFIX.len()..cursor + ESCAPE_LEN)
                .unwrap_or("")
                .to_owned();
            upper += digits.chars().filter(|c| ('A'..='F').contains(c)).count();
            lower += digits.chars().filter(|c| ('a'..='f').contains(c)).count();
            known.insert(unit, digits);
            cursor += ESCAPE_LEN;
        } else {
            cursor += 1;
        }
    }
    let mut out = String::new();
    for c in text.chars() {
        let mut units = [0_u16; 2];
        if c.is_ascii() {
            out.push(c);
            continue;
        }
        for unit in c.encode_utf16(&mut units).iter() {
            let digits = known.get(unit).cloned().unwrap_or_else(|| {
                let hex = format!("{unit:04x}");
                if upper > lower {
                    hex.to_uppercase()
                } else {
                    hex
                }
            });
            out.push_str("\\u");
            out.push_str(&digits);
        }
    }
    out
}

fn replace(content: &str, search: &str, with: &str, all: bool) -> String {
    if all {
        content.replace(search, with)
    } else {
        content.replacen(search, with, 1)
    }
}

/// 替换：删除整行时连同行尾换行一起删。
fn apply_once(content: &str, old: &str, new: &str, all: bool) -> String {
    if !new.is_empty() {
        return replace(content, old, new, all);
    }
    let with_newline = format!("{old}\n");
    if !old.ends_with('\n') && content.contains(&with_newline) {
        replace(content, &with_newline, new, all)
    } else {
        replace(content, old, new, all)
    }
}

/// 应用一次编辑；`old` 为空时整份内容换成 `new`。
pub(crate) fn apply(
    content: &str,
    old: &str,
    new: &str,
    all: bool,
) -> Result<String, &'static str> {
    if content.is_empty() && old.is_empty() && new.is_empty() {
        return Ok(String::new());
    }
    let updated = if old.is_empty() {
        new.to_owned()
    } else {
        apply_once(content, old, new, all)
    };
    // 单次编辑时先查“替换前后相同”，所以只会报这一句。
    if updated == content {
        return Err("String not found in file. Failed to apply edit.");
    }
    Ok(updated)
}

/// 编辑能否套上（"applies"）：找得到，且不要求唯一或只有一处。
pub(crate) fn applies(content: &str, old: &str, all: bool) -> bool {
    find_actual(content, old)
        .is_some_and(|actual| all || content.matches(actual.as_str()).count() <= 1)
}

#[cfg(test)]
#[path = "edit_match_tests.rs"]
mod tests;
