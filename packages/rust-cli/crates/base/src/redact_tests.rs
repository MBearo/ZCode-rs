// 复制自 rust/crates/kit/src/redact_tests.rs（feat/rs 79b6691c55）。
use std::borrow::Cow;

use super::*;

#[test]
fn redacts_bearer_tokens() {
    let cases = [
        (
            "Bearer abcde+fghijklmnopqrstuvwxyz012345",
            "Bearer [REDACTED]",
        ),
        (
            "Bearer sk-abcdefghijklmnopqrst+secret_suffix",
            "Bearer [REDACTED]",
        ),
        (
            "Bearer AbcdefghijklMN09._~+/-==; echo done",
            "Bearer [REDACTED]; echo done",
        ),
        (
            "authorization: bEaReR\tabcdefghijklmnop",
            "authorization: Bearer [REDACTED]",
        ),
        ("Bearer   abcdefghijklmnop", "Bearer [REDACTED]"),
    ];
    for (input, expected) in cases {
        assert_eq!(redact_secrets(input), expected, "input {input:?}");
    }
}

#[test]
fn avoids_bearer_false_positives() {
    for input in [
        "Bearer of good news",
        "Bearer abcdefghijklmno",
        "NotABearer abcdefghijklmnop",
        "Bearerabcdefghijklmnop",
        "Bearer\nabcdefghijklmnop",
        "Bearer\u{a0}abcdefghijklmnop",
        "Bearer abcdefghijklmno\u{212a}",
    ] {
        assert_eq!(redact_secrets(input), Cow::Borrowed(input));
    }
}

#[test]
fn redacts_provider_keys_and_tokens() {
    // 明确生成假 token，避免把形似真实凭据的字面量写入仓库。
    let fake_git_token = format!("gh ghp_{} ok", "0".repeat(36));
    let cases = [
        (
            "key sk-abcdefghijklmnopqrstuvwxyz done",
            "key [REDACTED] done",
        ),
        (
            "anthropic sk-ant-api03-abcdefghijklmnop_qrstu-vw",
            "anthropic [REDACTED]",
        ),
        ("openai sk-proj-abcdefghijklmnopqrstuv", "openai [REDACTED]"),
        ("aws AKIAIOSFODNN7EXAMPLE id", "aws [REDACTED] id"),
        (fake_git_token.as_str(), "gh [REDACTED] ok"),
        (
            "pat github_pat_11ABCDEFG0123456789_abcdefghijklmnop",
            "pat [REDACTED]",
        ),
        (
            "api_key=abcdefgh123 password: 'hunter2hunter2' TOKEN = \"tok_12345678\"",
            "api_key=[REDACTED] password: '[REDACTED]' TOKEN = \"[REDACTED]\"",
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(redact_secrets(input), expected, "input {input:?}");
    }
}

#[test]
fn leaves_ordinary_text_borrowed() {
    for input in [
        "",
        "task-abcdefghijklmnopqrstuvwxyz is a word, not a key",
        "AKIA123 is too short",
        "token=short",
        "the secret ingredient",
    ] {
        assert!(
            matches!(redact_secrets(input), Cow::Borrowed(text) if text == input),
            "input {input:?}"
        );
    }
}

/// 缺陷 R2-14：`\b` 把 `_` 当作单词字符，`access_token=`、`refresh_token=`、`client_secret=` 里关键字前面
/// 没有边界，原来不脱敏；也没有 `key=`、`code=`。现在关键字前面是非字母数字（或开头）就算边界。
#[test]
fn redacts_assignments_after_underscores_and_oauth_fields() {
    let cases = [
        (
            "access_token=abcdefgh1234&expires=3600",
            "access_token=[REDACTED]&expires=3600",
        ),
        (
            r#"{"refresh_token": "abcdefghij"}"#,
            r#"{"refresh_token": "[REDACTED]"}"#,
        ),
        ("client_secret=abcdefgh1234", "client_secret=[REDACTED]"),
        ("key=abcdefgh1234", "key=[REDACTED]"),
        (
            "/callback?code=abcdefgh1234&state=xyz",
            "/callback?code=[REDACTED]&state=xyz",
        ),
        ("x-api-key: abcdefgh1234", "x-api-key: [REDACTED]"),
    ];
    for (input, expected) in cases {
        assert_eq!(redact_secrets(input), expected, "input {input:?}");
    }
    // 关键字是别的词的一部分（前面是字母数字）时不算。
    for input in [
        "monkey=abcdefgh1234",
        "barcode=abcdefgh1234",
        "passcode: 12",
    ] {
        assert_eq!(
            redact_secrets(input),
            Cow::Borrowed(input),
            "input {input:?}"
        );
    }
}
