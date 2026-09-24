//! Node `sanitizeModelNetworkHeaders`, `inspectProviderFailure` and usage rules.
use super::*;

#[test]
fn headers_are_lowercased_and_credentials_redacted() {
    let headers = sanitize([
        ("X-Request-Id", "req"),
        ("Authorization", "Bearer secret"),
        ("X-Custom-Token", "t"),
        ("Cookie", "c"),
        ("x-api-key", "k"),
        ("User-Agent", "ZCode/1"),
        ("user-agent", "ZCode/2"),
    ]);
    assert_eq!(
        Value::from(headers),
        json!({"x-request-id": "req", "authorization": "[redacted]", "x-custom-token": "[redacted]",
            "cookie": "[redacted]", "x-api-key": "[redacted]", "user-agent": "ZCode/2"})
    );
    let mut map = reqwest::header::HeaderMap::new();
    map.append("set-cookie", "a".parse().unwrap());
    map.append("x-trace", "1".parse().unwrap());
    map.append("x-trace", "2".parse().unwrap());
    assert_eq!(
        Value::from(response_headers(&map)),
        json!({"set-cookie": "[redacted]", "x-trace": "1, 2"})
    );
}

#[test]
fn provider_details_come_from_the_error_body() {
    let body =
        json!({"error": {"code": 1302, "message": "  too   many\n requests ", "request_id": "r1"}});
    let detail = provider_detail(&body, Map::new());
    assert_eq!(detail.provider_error_code.as_deref(), Some("1302"));
    assert_eq!(
        detail.provider_error_message.as_deref(),
        Some("too many requests")
    );
    assert_eq!(detail.provider_request_id.as_deref(), Some("r1"));
    let long = provider_detail(
        &json!({"msg": "x".repeat(1200), "code": "PROVIDER_BUSINESS_ERROR"}),
        Map::new(),
    );
    assert_eq!(long.provider_error_message.unwrap().len(), 1003);
    assert_eq!(long.provider_error_code, None);
}

#[test]
fn usage_omits_what_the_provider_did_not_report() {
    assert_eq!(
        usage(&json!({"prompt_tokens": 10, "completion_tokens": 4})),
        json!({"inputTokens": 10, "outputTokens": 4, "totalTokens": 14})
    );
    assert_eq!(
        usage(&json!({"prompt_tokens": 10, "prompt_tokens_details": {"cached_tokens": 0}})),
        json!({"inputTokens": 10, "cacheReadTokens": 0})
    );
    assert_eq!(usage(&Value::Null), json!({}));
}

#[test]
fn timestamps_are_iso_with_milliseconds() {
    assert_eq!(iso(1_790_000_000_123), "2026-09-21T14:13:20.123Z");
}
