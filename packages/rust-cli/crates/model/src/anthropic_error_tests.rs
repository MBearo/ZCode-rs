use std::time::{Duration, UNIX_EPOCH};

use super::*;

fn api_error(kind: &str, message: &str) -> String {
    serde_json::json!({"type": "error", "error": {"type": kind, "message": message}}).to_string()
}

fn failure(status: u16, headers: &[(&str, &str)], body: &str) -> Failure {
    from_response(
        status,
        &ResponseHeaders::from_pairs(headers.iter().copied()),
        body,
        UNIX_EPOCH,
    )
}

#[test]
fn status_codes_map_to_error_kinds_and_retry_classes() {
    let rate = failure(
        429,
        &[("retry-after", "7")],
        &api_error("rate_limit_error", "slow down"),
    );
    assert_eq!(
        rate,
        Failure {
            error: ModelError::RateLimited {
                message: "slow down".to_owned(),
                retry_after_ms: Some(7000),
            },
            retry: Retry::Transient,
            retry_after: Some(Duration::from_secs(7)),
        }
    );
    let overloaded = failure(529, &[], &api_error("overloaded_error", "Overloaded"));
    assert_eq!(
        overloaded.error,
        ModelError::Overloaded {
            message: "Overloaded".to_owned()
        }
    );
    assert_eq!(overloaded.retry, Retry::Overloaded);
    let unauthorized = failure(
        401,
        &[],
        &api_error("authentication_error", "invalid x-api-key"),
    );
    assert!(matches!(
        unauthorized.error,
        ModelError::Authentication { .. }
    ));
    assert_eq!(unauthorized.retry, Retry::Transient);
    let forbidden = failure(403, &[], &api_error("permission_error", "no"));
    assert!(matches!(forbidden.error, ModelError::Authentication { .. }));
    assert_eq!(forbidden.retry, Retry::Never);
    let too_large = failure(
        413,
        &[],
        &api_error("request_too_large", "Request exceeds the maximum size"),
    );
    assert!(matches!(too_large.error, ModelError::InvalidRequest { .. }));
    assert_eq!(too_large.retry, Retry::Never);
    let bad = failure(
        400,
        &[],
        &api_error("invalid_request_error", "messages: required"),
    );
    assert!(matches!(bad.error, ModelError::InvalidRequest { .. }));
    assert_eq!(bad.retry, Retry::Never);
    for status in [408, 409] {
        assert_eq!(failure(status, &[], "").retry, Retry::Transient);
    }
    let server = failure(503, &[], "upstream connect error");
    assert_eq!(
        server.error,
        ModelError::Server {
            status: 503,
            message: "upstream connect error".to_owned()
        }
    );
    assert_eq!(server.retry, Retry::Transient);
    assert_eq!(
        failure(502, &[], "  ").error,
        ModelError::Server {
            status: 502,
            message: "HTTP 502".to_owned()
        }
    );
}

#[test]
fn prompt_too_long_is_recognized_on_400_and_413() {
    let direct = failure(
        400,
        &[],
        &api_error(
            "invalid_request_error",
            "prompt is too long: 210000 tokens > 200000 maximum",
        ),
    );
    assert_eq!(
        direct.error,
        ModelError::PromptTooLong {
            message: "prompt is too long: 210000 tokens > 200000 maximum".to_owned()
        }
    );
    assert_eq!(direct.retry, Retry::Never);
    // Vertex：413，首字母大写，正文不是 Anthropic 的错误形状。
    let vertex = failure(413, &[], "Prompt is too long");
    assert!(matches!(vertex.error, ModelError::PromptTooLong { .. }));
}

#[test]
fn overload_in_the_body_wins_over_the_status_and_headers() {
    // SDK 在流式请求里有时拿不到 529：正文里的 overloaded_error 也算过载，且不受 x-should-retry 影响。
    let hidden = failure(
        500,
        &[("x-should-retry", "false")],
        &api_error("overloaded_error", "Overloaded"),
    );
    assert!(matches!(hidden.error, ModelError::Overloaded { .. }));
    assert_eq!(hidden.retry, Retry::Overloaded);
}

#[test]
fn x_should_retry_overrides_the_status_rule() {
    let never = failure(
        500,
        &[("x-should-retry", "false")],
        &api_error("api_error", "boom"),
    );
    assert_eq!(never.retry, Retry::Never);
    let always = failure(
        400,
        &[("x-should-retry", "true")],
        &api_error("invalid_request_error", "x"),
    );
    assert_eq!(always.retry, Retry::Transient);
}

#[test]
fn messages_are_truncated_and_redacted() {
    let long = "x".repeat(MESSAGE_LIMIT + 10);
    let Failure { error, .. } = failure(500, &[], &long);
    let ModelError::Server { message, .. } = error else {
        panic!("{error:?}");
    };
    assert_eq!(message.chars().count(), MESSAGE_LIMIT);
    let leaked = failure(
        401,
        &[],
        &api_error(
            "authentication_error",
            "bad key sk-ant-api03-abcdefghijklmnopqrstuv",
        ),
    );
    assert!(
        !leaked.error.to_string().contains("abcdefghijklmnop"),
        "{:?}",
        leaked.error
    );
}

#[test]
fn stream_errors_use_the_status_of_their_type() {
    assert_eq!(
        from_stream(&api_error("overloaded_error", "Overloaded")),
        ModelError::Overloaded {
            message: "Overloaded".to_owned()
        }
    );
    assert_eq!(
        from_stream(&api_error("api_error", "Internal server error")),
        ModelError::Server {
            status: 500,
            message: "Internal server error".to_owned()
        }
    );
    assert!(matches!(
        from_stream(&api_error("rate_limit_error", "later")),
        ModelError::RateLimited {
            retry_after_ms: None,
            ..
        }
    ));
    assert!(matches!(
        from_stream(&api_error(
            "invalid_request_error",
            "prompt is too long: 5 tokens > 4 maximum"
        )),
        ModelError::PromptTooLong { .. }
    ));
    assert!(matches!(
        from_stream("not json"),
        ModelError::Server { status: 500, .. }
    ));
}
