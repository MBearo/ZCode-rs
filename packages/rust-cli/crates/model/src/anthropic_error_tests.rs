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

/// 兼容端点回 OpenAI 句式的超长（修复线 F108，model.md §5.1.1）：400 的原文。
const COMPATIBLE_OVERFLOW: &str = "This model's maximum context length is 1048576 tokens. However, you requested 1055008 tokens (671008 in the messages, 384000 in the completion).";

/// 兼容端点的超长说法也是上下文超限（触发反应式压缩），消息是原文、不重试。正文是 Anthropic 的错误形状或
/// OpenAI 的错误形状都认。
#[test]
fn an_openai_style_overflow_from_a_compatible_endpoint_is_prompt_too_long() {
    let anthropic_shape = failure(
        400,
        &[],
        &api_error("invalid_request_error", COMPATIBLE_OVERFLOW),
    );
    assert_eq!(
        anthropic_shape.error,
        ModelError::PromptTooLong {
            message: COMPATIBLE_OVERFLOW.to_owned()
        }
    );
    assert_eq!(anthropic_shape.retry, Retry::Never);
    let openai_shape = serde_json::json!({"error": {"message": COMPATIBLE_OVERFLOW,
        "type": "invalid_request_error", "param": null, "code": "invalid_request_error"}})
    .to_string();
    assert!(matches!(
        failure(400, &[], &openai_shape).error,
        ModelError::PromptTooLong { .. }
    ));
    // 流里的错误事件（`invalid_request_error` 按 400 算）。
    assert!(matches!(
        from_stream(&api_error("invalid_request_error", COMPATIBLE_OVERFLOW)),
        ModelError::PromptTooLong { .. }
    ));
}

/// 共用词表里另外几家服务的说法与错误码。
#[test]
fn the_shared_vocabulary_covers_other_providers() {
    for message in [
        "The input token count (1200000) exceeds the maximum number of tokens allowed (1048576).",
        "Range of input length should be [1, 129024]",
        "Total message token length exceed model limit",
        "context_window_exceeded",
        "model_context_window_exceeded",
        "Input exceeds the context window of this model",
    ] {
        let got = failure(400, &[], &api_error("invalid_request_error", message));
        assert!(
            matches!(got.error, ModelError::PromptTooLong { .. }),
            "{message}: {:?}",
            got.error
        );
    }
    let coded = serde_json::json!({"error": {"message": "too many tokens", "code": "context_length_exceeded"}})
        .to_string();
    assert!(matches!(
        failure(400, &[], &coded).error,
        ModelError::PromptTooLong { .. }
    ));
}

/// 不误伤：`invalid_request_error` 里别的内容、请求体过大、输出上限超出窗口照旧是 `InvalidRequest`；词表只在 4xx 上看，
/// 5xx 里出现同样的说法仍是服务端错误（`prompt is too long` 照旧不看状态码）。
#[test]
fn other_invalid_requests_are_not_mistaken_for_overflow() {
    for message in [
        "messages.0.content.1.image.source.base64: image exceeds 5 MB maximum: 7340032 bytes > 5242880 bytes",
        "messages.0.content.1.image.source.base64.data: At least one of the image dimensions exceed max allowed size for many-image requests: 2000 pixels",
        "max_tokens: 400000 > 128000, which is the maximum allowed number of output tokens for this model",
        "input length and `max_tokens` exceed context limit: 197000 + 21333 > 200000, decrease input length or `max_tokens` and try again",
        "messages: text content blocks must be non-empty",
    ] {
        let got = failure(400, &[], &api_error("invalid_request_error", message));
        assert!(
            matches!(got.error, ModelError::InvalidRequest { .. }),
            "{message}: {:?}",
            got.error
        );
    }
    let too_large = failure(
        413,
        &[],
        &api_error("request_too_large", "Request exceeds the maximum size"),
    );
    assert!(matches!(too_large.error, ModelError::InvalidRequest { .. }));
    let server = failure(500, &[], &api_error("api_error", COMPATIBLE_OVERFLOW));
    assert!(matches!(
        server.error,
        ModelError::Server { status: 500, .. }
    ));
    let stream = from_stream(&api_error("api_error", COMPATIBLE_OVERFLOW));
    assert!(matches!(stream, ModelError::Server { status: 500, .. }));
    // `prompt is too long` 不看状态码（与修复前相同）。
    let legacy = failure(
        500,
        &[],
        &api_error("api_error", "prompt is too long: 5 tokens > 4 maximum"),
    );
    assert!(matches!(legacy.error, ModelError::PromptTooLong { .. }));
}
