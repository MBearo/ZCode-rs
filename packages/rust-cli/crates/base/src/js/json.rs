// 复制自 rust/crates/kit/src/js/json.rs（feat/rs 79b6691c55）。
//! JavaScript `JSON.stringify` output for values stored in the shared Node
//! database (spec rust-m11-node-storage §2.3). `serde_json` keeps insertion
//! order (`preserve_order`); this writer adds what differs from JS:
//! array-index keys come first in ascending order, and numbers use the
//! ECMAScript `Number::toString` form (`1.0` → `1`, `1e21` → `1e+21`).
use serde_json::{Map, Number, Value};

// ── 字节的分类（`str_utf16_len`）──

/// 有短转义的控制字符 `\b` 与 `\f`（`\t`、`\n`、`\r` 写字节字面量）。
const BACKSPACE: u8 = 0x08;
const FORM_FEED: u8 = 0x0c;
/// C0 控制字符的最后一个（U+001F）；没有短转义的写成 `\u00xx`。
const C0_LAST: u8 = 0x1f;
/// ASCII 的最后一个（U+007F）。
const ASCII_LAST: u8 = 0x7f;
/// UTF-8 的续字节（`10xxxxxx`）。
const CONTINUATION_FIRST: u8 = 0x80;
const CONTINUATION_LAST: u8 = 0xbf;
/// UTF-8 两字节序列首字节的第一个与三字节序列首字节的最后一个；再往后是四字节序列的首字节。
const LEAD_2_FIRST: u8 = 0xc0;
const LEAD_3_LAST: u8 = 0xef;

// ── `JSON.stringify` 结果里的长度（UTF-16 码元）──

/// 短转义（`\"`、`\\`、`\n` 等）。
const SHORT_ESCAPE_LEN: u8 = 2;
/// `\u00xx`。
const UNICODE_ESCAPE_LEN: u8 = 6;
/// 四字节的 UTF-8 序列在 UTF-16 里是代理对。
const SURROGATE_PAIR_LEN: u8 = 2;
/// 字符串两侧的引号。
const QUOTES_LEN: usize = 2;
/// 数组的 `[]`、对象的 `{}`。
const BRACKETS_LEN: usize = 2;

// ── 数字 ──

/// 数组下标键最多的位数（最大的下标 2^32 − 2 = 4294967294 是 10 位）。
const ARRAY_INDEX_MAX_DIGITS: usize = 10;
/// `Number::toString`：小数点位置 n 不超过它时写定点形式，更大时写指数形式。
const FIXED_MAX_POINT: i32 = 21;
/// `Number::toString`：小数点位置 n 大于它（且不大于 0）时写 `0.00ddd`，再小时写指数形式。
const FIXED_MIN_POINT: i32 = -6;

/// `JSON.stringify(value)`.
pub fn stringify(value: &Value) -> String {
    let mut out = String::new();
    write(&mut out, value);
    out
}

/// Node `encodeJson`: `null` for JSON null, otherwise the JS text.
pub fn encode(value: &Value) -> Option<String> {
    (!value.is_null()).then(|| stringify(value))
}

/// `JSON.stringify(value).length`（UTF-16 码元数），不生成字符串。属性顺序不影响长度。
pub fn utf16_len(value: &Value) -> usize {
    match value {
        Value::Null => "null".len(),
        Value::Bool(b) => {
            if *b {
                "true".len()
            } else {
                "false".len()
            }
        }
        // 数字只含 ASCII。
        Value::Number(n) => number(n).len(),
        Value::String(s) => str_utf16_len(s),
        Value::Array(items) => {
            BRACKETS_LEN
                + items.len().saturating_sub(1)
                + items.iter().map(utf16_len).sum::<usize>()
        }
        Value::Object(map) => {
            BRACKETS_LEN
                + map.len().saturating_sub(1)
                + map
                    .iter()
                    .map(|(key, value)| str_utf16_len(key) + 1 + utf16_len(value))
                    .sum::<usize>()
        }
    }
}

/// `JSON.stringify(text).length`：两侧引号加转义后的 UTF-16 长度，转义规则同 [`write_str`]。
pub fn str_utf16_len(text: &str) -> usize {
    // 每个 UTF-8 字节对 UTF-16 长度的贡献：续字节 0，2/3 字节序列的首字节 1，4 字节序列的首字节 2
    //（代理对）；ASCII 按转义后的长度。
    const fn units(b: u8) -> u8 {
        match b {
            b'"' | b'\\' | BACKSPACE | b'\t' | b'\n' | FORM_FEED | b'\r' => SHORT_ESCAPE_LEN,
            0x00..=C0_LAST => UNICODE_ESCAPE_LEN,
            b' '..=ASCII_LAST => 1,
            CONTINUATION_FIRST..=CONTINUATION_LAST => 0,
            LEAD_2_FIRST..=LEAD_3_LAST => 1,
            _ => SURROGATE_PAIR_LEN,
        }
    }
    const TABLE: [u8; 256] = {
        let mut table = [0u8; 256];
        let mut b = 0;
        while b < 256 {
            table[b] = units(b as u8);
            b += 1;
        }
        table
    };
    QUOTES_LEN
        + text
            .bytes()
            .map(|b| usize::from(TABLE[usize::from(b)]))
            .sum::<usize>()
}

/// 把 `value` 按 `JSON.stringify` 写到 `out` 末尾（[`stringify`] 的写法，调用方可以接着写别的）。
pub fn write(out: &mut String, value: &Value) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&number(n)),
        Value::String(s) => write_str(out, s),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(out, item);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            let mut first = true;
            for (key, value) in properties(map) {
                if !first {
                    out.push(',');
                }
                first = false;
                write_str(out, key);
                out.push(':');
                write(out, value);
            }
            out.push('}');
        }
    }
}

/// JS 对象的属性遍历顺序：数组下标键按数值升序在前，其余按插入顺序。
pub fn properties(map: &Map<String, Value>) -> impl Iterator<Item = (&String, &Value)> {
    let mut index: Vec<(u32, &String, &Value)> = map
        .iter()
        .filter_map(|(k, v)| array_index(k).map(|i| (i, k, v)))
        .collect();
    index.sort_by_key(|(i, _, _)| *i);
    let rest = map.iter().filter(|(k, _)| array_index(k).is_none());
    index.into_iter().map(|(_, k, v)| (k, v)).chain(rest)
}

/// `JSON.stringify` 对合法 UTF-8 只转义 `"`、`\\` 与 U+0000–U+001F（有短形式的用短形式，其余小写
/// `\u00xx`），与 `serde_json` 相同。
///
/// 性能修复（修复线 F6，审查 R5-6）：以前每个字符串先经 `serde_json::to_string` 生成一个新 String 再拷进来，
/// 请求体与落库的 JSON 都要为每个字符串多分配一次；现在直接转义写进 `out`，输出逐字节不变（`json_tests.rs`）。
pub fn write_str(out: &mut String, value: &str) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    // 一个字节的低 4 位。
    const LOW_NIBBLE: u8 = 0x0f;
    let bytes = value.as_bytes();
    out.reserve(bytes.len() + QUOTES_LEN);
    out.push('"');
    let mut start = 0;
    for (at, &byte) in bytes.iter().enumerate() {
        // 查表（与 serde_json 的写法相同）：绝大多数字节不用转义，一次比较就过去。
        let escape = ESCAPE[usize::from(byte)];
        if escape == 0 {
            continue;
        }
        out.push_str(&value[start..at]);
        if escape == b'u' {
            out.push_str("\\u00");
            out.push(char::from(HEX[usize::from(byte >> 4)]));
            out.push(char::from(HEX[usize::from(byte & LOW_NIBBLE)]));
        } else {
            out.push('\\');
            out.push(char::from(escape));
        }
        start = at + 1;
    }
    out.push_str(&value[start..]);
    out.push('"');
}

/// 每个字节的转义：0 不转义，`u` 写成 `\u00xx`，其余是反斜杠后面的字符。
static ESCAPE: [u8; 256] = {
    let mut table = [0u8; 256];
    let mut byte = 0;
    while byte <= C0_LAST as usize {
        table[byte] = b'u';
        byte += 1;
    }
    table[BACKSPACE as usize] = b'b';
    table[b'\t' as usize] = b't';
    table[b'\n' as usize] = b'n';
    table[FORM_FEED as usize] = b'f';
    table[b'\r' as usize] = b'r';
    table[b'"' as usize] = b'"';
    table[b'\\' as usize] = b'\\';
    table
};

/// A canonical array index: `0` or no leading zero, below 2^32 − 1.
fn array_index(key: &str) -> Option<u32> {
    if key.is_empty()
        || key.len() > ARRAY_INDEX_MAX_DIGITS
        || (key.len() > 1 && key.starts_with('0'))
    {
        return None;
    }
    if !key.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    key.parse::<u64>()
        .ok()
        .filter(|v| *v < u64::from(u32::MAX))
        .map(|v| v as u32)
}

const SAFE_INTEGER: u64 = (1 << 53) - 1;

fn number(n: &Number) -> String {
    if let Some(v) = n.as_u64().filter(|v| *v <= SAFE_INTEGER) {
        return v.to_string();
    }
    if let Some(v) = n.as_i64().filter(|v| v.unsigned_abs() <= SAFE_INTEGER) {
        return v.to_string();
    }
    // JS 只有双精度：超过安全整数的整数与所有小数都按 Number::toString 输出。
    js_double(n.as_f64().unwrap_or(f64::NAN))
}

/// ECMAScript `Number::toString(10)`; non-finite values stringify as `null`.
pub fn js_double(x: f64) -> String {
    if !x.is_finite() {
        return "null".into();
    }
    if x == 0.0 {
        return "0".into();
    }
    // Rust 的 `{:e}` 给出可往返的最短有效数字，再按 JS 规则排版。
    let formatted = format!("{:e}", x.abs());
    let (mantissa, exponent) = formatted.split_once('e').unwrap_or((&formatted, "0"));
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let k = digits.len() as i32;
    let n = exponent + 1;
    let body = if k <= n && n <= FIXED_MAX_POINT {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= FIXED_MAX_POINT {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if FIXED_MIN_POINT < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let sign = if n > 0 { '+' } else { '-' };
        let tail = if k > 1 {
            format!(".{}", &digits[1..])
        } else {
            String::new()
        };
        format!("{}{tail}e{sign}{}", &digits[..1], (n - 1).abs())
    };
    if x < 0.0 { format!("-{body}") } else { body }
}

#[cfg(test)]
#[path = "json_tests.rs"]
mod tests;
