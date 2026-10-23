// 复制自 rust/crates/kit/src/js/json.rs（feat/rs 79b6691c55）。
//! JavaScript `JSON.stringify` output for values stored in the shared Node
//! database (spec rust-m11-node-storage §2.3). `serde_json` keeps insertion
//! order (`preserve_order`); this writer adds what differs from JS:
//! array-index keys come first in ascending order, and numbers use the
//! ECMAScript `Number::toString` form (`1.0` → `1`, `1e21` → `1e+21`).
use serde_json::{Map, Number, Value};

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
        Value::Null => 4,
        Value::Bool(b) => {
            if *b {
                4
            } else {
                5
            }
        }
        // 数字只含 ASCII。
        Value::Number(n) => number(n).len(),
        Value::String(s) => str_utf16_len(s),
        Value::Array(items) => {
            2 + items.len().saturating_sub(1) + items.iter().map(utf16_len).sum::<usize>()
        }
        Value::Object(map) => {
            2 + map.len().saturating_sub(1)
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
            b'"' | b'\\' | 0x08 | 0x09 | 0x0a | 0x0c | 0x0d => 2,
            0x00..=0x1f => 6,
            0x20..=0x7f => 1,
            0x80..=0xbf => 0,
            0xc0..=0xef => 1,
            _ => 2,
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
    2 + text
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
    let bytes = value.as_bytes();
    out.reserve(bytes.len() + 2);
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
            out.push(char::from(HEX[usize::from(byte & 0xf)]));
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
    while byte < 0x20 {
        table[byte] = b'u';
        byte += 1;
    }
    table[0x08] = b'b';
    table[0x09] = b't';
    table[0x0a] = b'n';
    table[0x0c] = b'f';
    table[0x0d] = b'r';
    table[b'"' as usize] = b'"';
    table[b'\\' as usize] = b'\\';
    table
};

/// A canonical array index: `0` or no leading zero, below 2^32 − 1.
fn array_index(key: &str) -> Option<u32> {
    if key.is_empty() || key.len() > 10 || (key.len() > 1 && key.starts_with('0')) {
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
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
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
