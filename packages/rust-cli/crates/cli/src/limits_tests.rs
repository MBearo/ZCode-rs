//! 跑分用的三个开关的读法（cli-args.md §9，修复线 F42）。

use super::*;

fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

/// `js_int`：parseInt、科学计数法（整数才算）、千分位、首尾空白。
#[test]
fn values_parse_as_integers() {
    assert_eq!(js_int("4096"), Some(4096.0));
    assert_eq!(js_int("  4096 \n"), Some(4096.0));
    assert_eq!(js_int("250ms"), Some(250.0));
    assert_eq!(js_int("+12"), Some(12.0));
    assert_eq!(js_int("-5"), Some(-5.0));
    assert_eq!(js_int("1.9"), Some(1.0));
    assert_eq!(js_int("1e4"), Some(10_000.0));
    assert_eq!(js_int("2.5E3"), Some(2_500.0));
    assert_eq!(js_int(".5e1"), Some(5.0));
    assert_eq!(js_int("1.e3"), Some(1_000.0));
    assert_eq!(js_int("1.5e0"), None);
    assert_eq!(js_int("64,000"), Some(64_000.0));
    assert_eq!(js_int("1_000_000"), Some(1_000_000.0));
    assert_eq!(js_int("1 000"), Some(1_000.0));
    assert_eq!(js_int("1\u{a0}000"), Some(1_000.0));
    // 分隔符不一致、组不是三位：不是千分位，按 parseInt 取开头的数字。
    assert_eq!(js_int("1,000_000"), Some(1.0));
    assert_eq!(js_int("1234,567"), Some(1234.0));
    assert_eq!(js_int("lots"), None);
    assert_eq!(js_int(""), None);
    assert_eq!(js_int("-"), None);
}

/// 首尾空白按 JS 的 `String.prototype.trim`（base `js::string::trim`）：去掉 U+FEFF，不去 U+0085（NEL）。
/// 修复线 F58：cli 原来自己写的 `js_trim` 用 Rust 的 `char::is_whitespace`，会去掉 U+0085，`"\u{85}4096"` 读成
/// 4096；JS 里 trim 后仍以 U+0085 开头，`parseInt` 是 NaN。
#[test]
fn surrounding_whitespace_is_the_javascript_trim_set() {
    assert_eq!(js_int("\u{feff}4096\u{feff}"), Some(4096.0));
    assert_eq!(js_int("\u{3000}4096\u{2028}"), Some(4096.0));
    assert_eq!(js_int("\u{85}4096"), None);
    assert_eq!(js_int("\u{85}1e4"), None);
}

/// `ZCODE_MAX_OUTPUT_TOKENS`：正整数才算；旧名字不读。
#[test]
fn the_output_limit_reads_the_zcode_name() {
    let read = |value: &str| max_output_tokens(&env(&[("ZCODE_MAX_OUTPUT_TOKENS", value)]));
    assert_eq!(max_output_tokens(&[]), None);
    assert_eq!(read(""), None);
    assert_eq!(read("4096"), Some(4096));
    assert_eq!(read("1e6"), Some(1_000_000));
    assert_eq!(read("0"), None);
    assert_eq!(read("-1"), None);
    assert_eq!(read("lots"), None);
    assert_eq!(read("99999999999"), Some(u32::MAX));
}

/// `MAX_THINKING_TOKENS`：大于 0 是预算，其余非空的值都让 thinking 关。
#[test]
fn max_thinking_tokens_sets_budget_or_off() {
    let read = |value: &str| thinking(&env(&[("MAX_THINKING_TOKENS", value)]));
    assert_eq!(thinking(&[]), ThinkingTokens::Unset);
    assert_eq!(read(""), ThinkingTokens::Unset);
    assert_eq!(read("8000"), ThinkingTokens::Budget(8000));
    assert_eq!(read("0"), ThinkingTokens::Disabled);
    assert_eq!(read("-1"), ThinkingTokens::Disabled);
    assert_eq!(read("off"), ThinkingTokens::Disabled);
    assert_eq!(read("  "), ThinkingTokens::Disabled);
}

/// `BASH_*_TIMEOUT_MS`：`ud` 大于 0 才算设了，之后按 `BashTimeouts::new` 推导。
#[test]
fn bash_timeouts_read_both_variables() {
    assert_eq!(bash(&[]), BashTimeouts::default());
    assert_eq!(
        bash(&env(&[
            ("BASH_DEFAULT_TIMEOUT_MS", "250ms"),
            ("BASH_MAX_TIMEOUT_MS", "900ms"),
        ])),
        BashTimeouts::new(Some(250), Some(900))
    );
    assert_eq!(
        bash(&env(&[
            ("BASH_DEFAULT_TIMEOUT_MS", "invalid"),
            ("BASH_MAX_TIMEOUT_MS", "0"),
        ])),
        BashTimeouts::default()
    );
    // 输出上限由入口读一次传进来（F58），这里不再读变量。
    let all = limits(
        &env(&[
            ("ZCODE_MAX_OUTPUT_TOKENS", "1"),
            ("MAX_THINKING_TOKENS", "0"),
            ("BASH_DEFAULT_TIMEOUT_MS", "5000"),
        ]),
        Some(64_000),
    );
    assert_eq!(all.request.max_output_tokens, Some(64_000));
    assert_eq!(all.request.thinking, ThinkingTokens::Disabled);
    assert_eq!(all.bash.default_ms, 5_000);
}
