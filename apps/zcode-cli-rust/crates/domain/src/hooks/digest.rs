//! Workspace hook digests (Node `workspace-hook-digest.ts`): `sha256` of the
//! `JSON.stringify` payload, numbers written the JavaScript way.
use super::{HookEvent, timeout_ms};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// JS `Number#toString` (the digest payloads are `JSON.stringify` output).
pub fn js_number(value: f64) -> String {
    if value == 0.0 {
        return "0".into();
    }
    if !value.is_finite() {
        return "null".into();
    }
    // Rust `{:e}` 给出最短往返的有效数字与指数，再按 ECMAScript 规则排版。
    let formatted = format!("{:e}", value.abs());
    let (mantissa, exponent) = formatted.split_once('e').unwrap();
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let n = exponent.parse::<i32>().unwrap() + 1;
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let exp = n - 1;
        let sign = if exp < 0 { '-' } else { '+' };
        let head = if k == 1 {
            digits.clone()
        } else {
            format!("{}.{}", &digits[..1], &digits[1..])
        };
        format!("{head}e{sign}{}", exp.abs())
    };
    if value < 0.0 {
        format!("-{body}")
    } else {
        body
    }
}

/// `JSON.stringify` of a digest payload: numbers in JS form, strings and
/// arrays as serde writes them (both escape the same way).
fn stringify(value: &Value) -> String {
    match value {
        Value::Number(n) => n.as_f64().map(js_number).unwrap_or_else(|| n.to_string()),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(stringify).collect();
            format!("[{}]", parts.join(","))
        }
        other => other.to_string(),
    }
}

pub(super) fn sha256(payload: &Value) -> String {
    let digest = Sha256::digest(stringify(payload).as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

pub(super) fn optional(value: &Value) -> Value {
    if value.is_null() {
        json!(["unset"])
    } else {
        json!(["set", value])
    }
}

/// Where a declaration sits (the digest's identity fields).
pub struct Slot<'a> {
    pub relative_path: &'a str,
    pub discovery_order: usize,
    pub event: HookEvent,
    /// The raw `matcher` (`null` when absent).
    pub matcher: &'a Value,
    pub matcher_index: usize,
    pub hook_index: usize,
    /// Timeout used when the declaration sets none.
    pub default_timeout_ms: u64,
    pub max_output_bytes: u64,
}

/// Node `createWorkspaceHookDeclarationDigest` over the raw (untrimmed) hook.
pub fn declaration_digest(raw: &Value, slot: &Slot) -> String {
    let execution = if raw["type"] == "process" {
        json!([
            "process",
            raw["command"],
            raw["args"].as_array().cloned().unwrap_or_default()
        ])
    } else {
        let shell = match &raw["shell"] {
            Value::Null => json!(["unset"]),
            Value::Bool(true) => json!(["true"]),
            other => json!(["string", other]),
        };
        json!(["command", raw["command"], raw["async"] == true, shell])
    };
    sha256(&json!([
        "workspace-hook-declaration",
        1,
        slot.relative_path,
        slot.discovery_order,
        slot.event.as_str(),
        slot.matcher,
        slot.matcher_index,
        slot.hook_index,
        execution,
        timeout_ms(raw, slot.default_timeout_ms as f64),
        slot.max_output_bytes
    ]))
}
