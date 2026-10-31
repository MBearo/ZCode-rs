//! 交给内核的开关（docs/specs/rust-cli/cli-args.md §9，修复线 F42）：`ZCODE_MAX_OUTPUT_TOKENS`、`MAX_THINKING_TOKENS`、
//! `BASH_DEFAULT_TIMEOUT_MS` / `BASH_MAX_TIMEOUT_MS`，工作线 F89 加 `ENABLE_TOOL_SEARCH`（tools-more.md §3.2.1）。值按 JS 风格
//! 的整数读法（`js_int`）。

use rust_cli_prompts::models::ToolSearchSetting;
use rust_cli_runtime::{BashTimeouts, Limits, RequestLimits, ThinkingTokens};

use crate::consts;

/// 环境变量的原值（不去空白）；JS 的 `process.env.X` 为空串时是假值，与没设相同。
fn raw<'a>(env: &'a [(String, String)], name: &str) -> Option<&'a str> {
    rust_cli_base::secret::env_var(env, name).filter(|value| !value.is_empty())
}

/// JS `parseInt(text, 10)`：可选的正负号，之后开头的十进制数字；没有数字时是 NaN（None）。
fn parse_int(text: &str) -> Option<f64> {
    let (negative, rest) = match text.as_bytes().first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let value: f64 = rest[..end].parse().ok()?;
    Some(if negative { -value } else { value })
}

/// 科学计数法 `^[+-]?(\d+(\.\d*)?|\.\d+)[eE][+-]?\d+$`。
fn is_scientific(text: &str) -> bool {
    let body = text.strip_prefix(['+', '-']).unwrap_or(text);
    let Some((mantissa, exponent)) = body.split_once(['e', 'E']) else {
        return false;
    };
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    let mantissa_ok = match mantissa.split_once('.') {
        None => digits(mantissa),
        Some((whole, fraction)) => {
            (digits(whole) && (fraction.is_empty() || digits(fraction)))
                || (whole.is_empty() && digits(fraction))
        }
    };
    let exponent = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
    mantissa_ok && digits(exponent)
}

/// 千分位写法 `^[+-]?\d{1,3}([_,   ])\d{3}(?:\1\d{3})*$`（各组用同一个分隔符）。
fn grouped(text: &str) -> Option<String> {
    let (sign, body) = match text.chars().next() {
        Some(c @ ('+' | '-')) => (Some(c), &text[1..]),
        _ => (None, text),
    };
    let lead = body.find(|c: char| !c.is_ascii_digit())?;
    if !(1..=consts::DIGIT_GROUP_LEN).contains(&lead) {
        return None;
    }
    let separator = body[lead..].chars().next()?;
    if !matches!(separator, '_' | ',' | '\u{a0}' | '\u{202f}' | ' ') {
        return None;
    }
    let mut digits = body[..lead].to_owned();
    for group in body[lead..].split(separator).skip(1) {
        if group.len() != consts::DIGIT_GROUP_LEN || !group.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        digits.push_str(group);
    }
    Some(sign.map_or(digits.clone(), |sign| format!("{sign}{digits}")))
}

/// JS 风格的整数读法：去掉首尾空白；32 个码元以内的科学计数法取 `Number`（不是整数时
/// 是 NaN），千分位写法去掉分隔符后 `parseInt`；否则 `parseInt`。None 是 NaN。
pub(crate) fn js_int(text: &str) -> Option<f64> {
    // 修复线 F58：以前用自己的 `js_trim`（Rust 空白 + U+FEFF），会去掉 JS 不去的 U+0085；改用 base 唯一的 JS trim。
    let text = rust_cli_base::js::string::trim(text);
    if text.encode_utf16().count() <= consts::SHORT_NUMBER_LEN {
        if is_scientific(text) {
            let value: f64 = text.parse().ok()?;
            return (value.is_finite() && value.fract() == 0.0).then_some(value);
        }
        if let Some(digits) = grouped(text) {
            return parse_int(&digits);
        }
    }
    parse_int(text)
}

/// 正数取整到 u64（值已是整数；JS 的数可能超出 u64，`as` 饱和到 u64::MAX）。
fn positive(value: Option<f64>) -> Option<u64> {
    value.filter(|value| *value > 0.0).map(|value| value as u64)
}

/// `ZCODE_MAX_OUTPUT_TOKENS`：没设或空时 None；`js_int` 不是正数时也是 None（用缺省，记一条 warn）。只在入口
/// `capture` 调一次（F58）。
pub(crate) fn max_output_tokens(env: &[(String, String)]) -> Option<u32> {
    let value = raw(env, rust_cli_base::names::env::ZCODE_MAX_OUTPUT_TOKENS)?;
    let parsed = positive(js_int(value));
    if parsed.is_none() {
        tracing::warn!(
            event = "env.invalid",
            name = rust_cli_base::names::env::ZCODE_MAX_OUTPUT_TOKENS,
            value,
            "invalid value, using the model default"
        );
    }
    parsed.map(|value| u32::try_from(value).unwrap_or(u32::MAX))
}

/// `MAX_THINKING_TOKENS`（启动时的 thinking 配置）：没设或空 → 按模型；`js_int` 大于 0 → 预算；其余 → 关。
pub(crate) fn thinking(env: &[(String, String)]) -> ThinkingTokens {
    match raw(env, rust_cli_base::names::env::MAX_THINKING_TOKENS) {
        None => ThinkingTokens::Unset,
        Some(value) => match positive(js_int(value)) {
            Some(budget) => ThinkingTokens::Budget(u32::try_from(budget).unwrap_or(u32::MAX)),
            None => ThinkingTokens::Disabled,
        },
    }
}

/// `auto` 与读不出数的 `auto:N` 的门槛（上下文窗口的百分之几）。
const AUTO_PERCENT: u8 = 10;
/// `auto:N` 的 N 到这么大时关掉 ToolSearch（百分比）。
const OFF_PERCENT: f64 = rust_cli_base::units::PERCENT_F64;

/// `ENABLE_TOOL_SEARCH`（工作线 F89，tools-more.md §3.2.1）：只决定 ToolSearch 开不开，开时一律本地。没设、空串、真值与
/// 不认识的值是开；`0` / `false` / `no` / `off`（去首尾空白、不分大小写）是关；`auto` 与 `auto:N` 按原值比较（不去空白、
/// 区分大小写），N 按 `js_int` 读、夹到 0–100：0 是开，100 是关，其余自动；读不出时自动、门槛 10%。
pub(crate) fn tool_search(env: &[(String, String)]) -> ToolSearchSetting {
    let Some(value) = raw(env, rust_cli_base::names::env::ENABLE_TOOL_SEARCH) else {
        return ToolSearchSetting::On;
    };
    if let Some(rest) = value.strip_prefix("auto:") {
        let Some(percent) = js_int(rest) else {
            tracing::warn!(
                event = "env.invalid",
                name = rust_cli_base::names::env::ENABLE_TOOL_SEARCH,
                value,
                "expected auto:N where N is a number, using auto"
            );
            return ToolSearchSetting::Auto {
                percent: AUTO_PERCENT,
            };
        };
        return match percent {
            percent if percent <= 0.0 => ToolSearchSetting::On,
            percent if percent >= OFF_PERCENT => ToolSearchSetting::Off,
            percent => ToolSearchSetting::Auto {
                percent: percent as u8,
            },
        };
    }
    if value == "auto" {
        return ToolSearchSetting::Auto {
            percent: AUTO_PERCENT,
        };
    }
    match rust_cli_base::js::string::trim(value)
        .to_lowercase()
        .as_str()
    {
        "0" | "false" | "no" | "off" => ToolSearchSetting::Off,
        _ => ToolSearchSetting::On,
    }
}

/// `BASH_DEFAULT_TIMEOUT_MS` / `BASH_MAX_TIMEOUT_MS`：`js_int` 大于 0 才算设了。
pub(crate) fn bash(env: &[(String, String)]) -> BashTimeouts {
    let millis = |name: &str| positive(raw(env, name).and_then(js_int));
    BashTimeouts::new(
        millis(rust_cli_base::names::env::BASH_DEFAULT_TIMEOUT_MS),
        millis(rust_cli_base::names::env::BASH_MAX_TIMEOUT_MS),
    )
}

/// 交给内核的全部开关（runtime `Defaults.limits`）。`max_output_tokens` 是入口读一次的值（修复线 F58：以前模型端口、
/// 平台层、压缩与这里各读一次，无效值的警告写 4 次）。
pub(crate) fn limits(env: &[(String, String)], max_output_tokens: Option<u32>) -> Limits {
    Limits {
        request: RequestLimits {
            max_output_tokens,
            thinking: thinking(env),
            tool_search: tool_search(env),
        },
        bash: bash(env),
        fork_subagent: fork_subagent(env),
    }
}

/// `ZCODE_FORK_SUBAGENT`（修复线 L-A，agents.md §8.1.1；真假值的写法同对照实现）：真值打开，假值关掉，
/// 别的值与没设都按形态。
pub(crate) fn fork_subagent(env: &[(String, String)]) -> Option<bool> {
    let value = raw(env, rust_cli_base::names::env::ZCODE_FORK_SUBAGENT)?;
    match rust_cli_base::js::string::trim(value)
        .to_lowercase()
        .as_str()
    {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
#[path = "limits_tests.rs"]
mod tests;
