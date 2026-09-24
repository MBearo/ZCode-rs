//! JavaScript `JSON.stringify` output for values stored in the shared Node
//! database (spec rust-m11-node-storage §2.3). `serde_json` keeps insertion
//! order (`preserve_order`); this writer adds what differs from JS:
//! array-index keys come first in ascending order, and numbers use the
//! ECMAScript `Number::toString` form (`1.0` → `1`, `1e21` → `1e+21`).
use serde_json::{Number, Value};

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

fn write(out: &mut String, value: &Value) {
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
            // JS 对象的属性遍历顺序：数组下标键按数值升序在前，其余按插入顺序。
            let mut index: Vec<(u32, &String, &Value)> = map
                .iter()
                .filter_map(|(k, v)| array_index(k).map(|i| (i, k, v)))
                .collect();
            index.sort_by_key(|(i, _, _)| *i);
            let rest = map.iter().filter(|(k, _)| array_index(k).is_none());
            out.push('{');
            let mut first = true;
            for (key, value) in index.into_iter().map(|(_, k, v)| (k, v)).chain(rest) {
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

/// `serde_json` escapes exactly what `JSON.stringify` escapes for valid
/// UTF-8: `"`, `\` and U+0000–U+001F (short forms, else lowercase `\u00xx`).
fn write_str(out: &mut String, value: &str) {
    out.push_str(&serde_json::to_string(value).unwrap_or_default());
}

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
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn numbers_follow_ecmascript() {
        let cases = [
            (1.0, "1"),
            (-0.0, "0"),
            (0.1, "0.1"),
            (1.5, "1.5"),
            (123_456_789.0, "123456789"),
            (1e21, "1e+21"),
            (1.5e21, "1.5e+21"),
            (1e20, "100000000000000000000"),
            (0.000001, "0.000001"),
            (1e-7, "1e-7"),
            (2.5e-8, "2.5e-8"),
            (-3.25, "-3.25"),
            (f64::NAN, "null"),
        ];
        for (value, expected) in cases {
            assert_eq!(js_double(value), expected, "{value}");
        }
        let parsed: Value = serde_json::from_str(r#"[1.0,2e2,-0.0,9007199254740993]"#).unwrap();
        assert_eq!(stringify(&parsed), "[1,200,0,9007199254740992]");
    }

    #[test]
    fn objects_follow_js_property_order() {
        let parsed: Value =
            serde_json::from_str(r#"{"b":1,"10":2,"a":{"2":0,"x":1,"1":2},"01":3,"0":4}"#).unwrap();
        assert_eq!(
            stringify(&parsed),
            r#"{"0":4,"10":2,"b":1,"a":{"1":2,"2":0,"x":1},"01":3}"#
        );
        assert_eq!(
            stringify(&json!({"s":"a\"\\\n\u{1}\u{7f}/\u{2028}é"})),
            "{\"s\":\"a\\\"\\\\\\n\\u0001\u{7f}/\u{2028}é\"}"
        );
        assert_eq!(encode(&Value::Null), None);
        assert_eq!(encode(&json!({})).as_deref(), Some("{}"));
    }
}
