//! Monitor 的输入：zod 的细校验与 ws 地址的私有段判断（docs/specs/rust-cli/tools-more.md §19.3、§19.4）。
//!
//! 通用的 schema 校验（类型、必填、多余的键）在执行管线里先做；这里补 zod 才有的检查，报错照 zod 的 issue
//! 数组（`InputValidationError: [ … ]`，两格缩进，MO1 实测）。字段的检查先做，有问题时不做整体的“正好一个来源”。

use std::net::{Ipv4Addr, Ipv6Addr};

use serde::Deserialize;
use serde_json::{Value, json};

/// 期限的下限、上限与缺省（毫秒）。
pub(crate) const MIN_MS: u64 = 1_000;
pub(crate) const MAX_MS: u64 = 3_600_000;
pub(crate) const DEFAULT_MS: u64 = 300_000;

/// 子协议的写法（RFC 6455 的 token）。
const TOKEN_PATTERN: &str = "^[!#$%&'*+.^_`|~0-9A-Za-z-]+$";

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Input {
    pub description: String,
    #[serde(default)]
    pub timeout_ms: Option<f64>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub ws: Option<WsInput>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct WsInput {
    pub url: String,
    #[serde(default)]
    pub protocols: Option<Vec<String>>,
}

impl Input {
    /// 生效的期限：缺省 300000，再截到这个形态的上限。
    pub(crate) fn timeout(&self, cap_ms: u64) -> u64 {
        // 缺省与下限在校验里保证；小数的毫秒按整数用。
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "校验之后在 [1000, 3600000] 之内"
        )]
        let requested = self.timeout_ms.map_or(DEFAULT_MS, |ms| ms as u64);
        requested.min(cap_ms)
    }
}

/// 审批框里看不见、所以 `command` 与 `ws.url` 不许的字符：C0 控制字符（只有制表符与换行除外）、DEL 与 C1 控制字符
/// （tools-more.md §19.3）。修 bug（工作线 F95）：以前回车（0x0D）也放过；回车在终端里回到行首，后面的字盖住前面的字，
/// 对齐对象只放过制表符与换行，这里照它把回车算作控制字符。
fn has_hidden_controls(text: &str) -> bool {
    text.chars().any(|c| {
        let code = u32::from(c);
        (code < 0x20 && !matches!(c, '\t' | '\n')) || (0x7f..=0x9f).contains(&code)
    })
}

/// ws URL 的 refine：ASCII、`ws:` / `wss:`、没有用户名密码与空白。
fn valid_ws_url(url: &str) -> bool {
    if !url.is_ascii() || url.contains(['\t', '\n', '\r']) {
        return false;
    }
    match url::Url::parse(url) {
        Ok(parsed) => {
            matches!(parsed.scheme(), "ws" | "wss")
                && parsed.username().is_empty()
                && parsed.password().is_none()
        }
        Err(_) => false,
    }
}

fn token(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+.^_`|~-".contains(c))
}

fn custom(path: Value, message: &str) -> Value {
    json!({"code": "custom", "path": path, "message": message})
}

/// zod 的 issue 数组；没有问题时为 None。
pub(crate) fn issues(input: &Value) -> Option<Vec<Value>> {
    let mut issues = Vec::new();
    if let Some(timeout) = input.get("timeout_ms").and_then(Value::as_f64) {
        if timeout < MIN_MS as f64 {
            issues.push(json!({
                "origin": "number", "code": "too_small", "minimum": MIN_MS, "inclusive": true,
                "path": ["timeout_ms"], "message": format!("Too small: expected number to be >={MIN_MS}"),
            }));
        } else if timeout > MAX_MS as f64 {
            issues.push(json!({
                "origin": "number", "code": "too_big", "maximum": MAX_MS, "inclusive": true,
                "path": ["timeout_ms"], "message": format!("timeout_ms must be \u{2264} {MAX_MS}"),
            }));
        }
    }
    if let Some(command) = input.get("command").and_then(Value::as_str)
        && has_hidden_controls(command)
    {
        issues.push(custom(
            json!(["command"]),
            "command contains control characters that would be hidden in the approval dialog",
        ));
    }
    if let Some(ws) = input.get("ws") {
        if let Some(url) = ws.get("url").and_then(Value::as_str) {
            if has_hidden_controls(url) {
                issues.push(custom(
                    json!(["ws", "url"]),
                    "url contains control characters that would be hidden in the approval dialog",
                ));
            }
            if !valid_ws_url(url) {
                issues.push(custom(
                    json!(["ws", "url"]),
                    "url must be a valid ASCII ws:// or wss:// URL with no userinfo or whitespace",
                ));
            }
        }
        if let Some(protocols) = ws.get("protocols").and_then(Value::as_array) {
            let mut fine = true;
            for (index, protocol) in protocols.iter().enumerate() {
                if protocol.as_str().is_some_and(|text| !token(text)) {
                    fine = false;
                    issues.push(json!({
                        "origin": "string", "code": "invalid_format", "format": "regex",
                        "pattern": format!("/{TOKEN_PATTERN}/"), "path": ["ws", "protocols", index],
                        "message": "protocol must be an RFC 6455 token",
                    }));
                }
            }
            let mut seen: Vec<&Value> = Vec::new();
            let unique = protocols.iter().all(|protocol| {
                let fresh = !seen.contains(&protocol);
                seen.push(protocol);
                fresh
            });
            if fine && !unique {
                issues.push(custom(
                    json!(["ws", "protocols"]),
                    "protocols must be unique",
                ));
            }
        }
    }
    if issues.is_empty() {
        let sources = [input.get("command"), input.get("ws")]
            .iter()
            .filter(|value| value.is_some_and(|value| !value.is_null()))
            .count();
        if sources != 1 {
            issues.push(custom(json!([]), "exactly one of command or ws"));
        }
    }
    (!issues.is_empty()).then_some(issues)
}

/// `InputValidationError: [ … ]`（JSON.stringify 的两格缩进）。
pub(crate) fn validation_message(issues: &[Value]) -> String {
    let text = serde_json::to_string_pretty(&Value::Array(issues.to_vec())).unwrap_or_default();
    format!("InputValidationError: {text}")
}

/// IPv4 的私有、链路本地与云元数据段（127/8 不算）。
fn private_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    match a {
        127 => false,
        0 | 10 => true,
        169 => b == 254,
        172 => (16..=31).contains(&b),
        100 => (64..=127).contains(&b),
        192 => b == 168,
        _ => false,
    }
}

/// IPv6：`::1` 不算，`::`、`fc00::/7`、`fe80::/10` 与映射的 IPv4 按上面。
fn private_v6(ip: Ipv6Addr) -> bool {
    if ip == Ipv6Addr::LOCALHOST {
        return false;
    }
    if ip.is_unspecified() {
        return true;
    }
    if let Some(v4) = ip.to_ipv4_mapped() {
        return private_v4(v4);
    }
    let first = ip.segments()[0];
    (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
}

/// ws URL 的主机是 IP 字面量且在私有段时返回主机（去掉方括号），否则 None。
pub(crate) fn private_host(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    match parsed.host()? {
        url::Host::Ipv4(ip) => private_v4(ip).then(|| ip.to_string()),
        url::Host::Ipv6(ip) => private_v6(ip).then(|| ip.to_string()),
        url::Host::Domain(_) => None,
    }
}

#[cfg(test)]
#[path = "monitor_input_tests.rs"]
mod tests;
