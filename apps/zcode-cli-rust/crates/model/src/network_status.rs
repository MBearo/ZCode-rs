//! Node model network status events (`ModelNetworkStatusEvent`, adapters
//! `runner-status.ts` / `runner-network-headers.ts`): one context per physical
//! attempt, headers shown sanitized, payloads in Node's field set.
use super::config::ModelConfig;
use super::model_protocol::ApiType;
use crate::contract::{ModelFailure, ModelOutput, RequestOrigin};
use serde_json::{Map, Value, json};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::Instant;
use zcode_cli_domain::model::FailureDetail;

const REDACTED: [&str; 8] = [
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "x-api-key",
    "api-key",
    "openai-api-key",
    "x-off-peak-ticket-id",
];
const REDACTED_PARTS: [&str; 5] = ["authorization", "api-key", "token", "secret", "cookie"];
const PROVIDER_REQUEST_HEADERS: [&str; 5] = [
    "x-request-id",
    "request-id",
    "x-amzn-requestid",
    "x-amz-request-id",
    "cf-ray",
];

/// Node `sanitizeModelNetworkHeaders`: lowercase names, later value wins,
/// credentials replaced by `"[redacted]"`.
pub(crate) fn sanitize<'a>(
    headers: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Map<String, Value> {
    let mut out = Map::new();
    for (name, value) in headers {
        let name = name.to_ascii_lowercase();
        let secret =
            REDACTED.contains(&name.as_str()) || REDACTED_PARTS.iter().any(|p| name.contains(p));
        let value = if secret { "[redacted]" } else { value };
        out.insert(name, value.into());
    }
    out
}

/// Response headers as fetch `Headers` yields them: repeated values joined by `", "`.
pub(crate) fn response_headers(headers: &reqwest::header::HeaderMap) -> Map<String, Value> {
    let mut joined: Vec<(String, String)> = vec![];
    for (name, value) in headers {
        let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
        match joined.iter_mut().find(|(n, _)| n == name.as_str()) {
            Some((_, existing)) => {
                existing.push_str(", ");
                existing.push_str(&value);
            }
            None => joined.push((name.as_str().to_owned(), value)),
        }
    }
    sanitize(joined.iter().map(|(n, v)| (n.as_str(), v.as_str())))
}

/// Node `inspectProviderFailure` over a provider error body.
pub(crate) fn provider_detail(body: &Value, headers: Map<String, Value>) -> FailureDetail {
    let error = body.get("error").filter(|e| e.is_object());
    let find = |keys: &[&str]| {
        [Some(body), error].into_iter().flatten().find_map(|scope| {
            keys.iter().find_map(|key| match &scope[*key] {
                Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
        })
    };
    let message = find(&["msg", "message"])
        .or_else(|| body["error"].as_str().map(str::to_owned))
        .map(|m| {
            let collapsed = m.split_whitespace().collect::<Vec<_>>().join(" ");
            match collapsed.char_indices().nth(1000) {
                Some((at, _)) => format!("{}...", &collapsed[..at]),
                None => collapsed,
            }
        });
    FailureDetail {
        response_headers: headers,
        provider_error_code: find(&["providerCode", "error_code", "code"])
            .filter(|c| c != "PROVIDER_BUSINESS_ERROR"),
        provider_error_message: message,
        provider_request_id: find(&["request_id", "requestId", "id"]),
    }
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn iso(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// One physical attempt of a model request.
#[derive(Clone)]
pub(crate) struct Attempt {
    pub number: u32,
    pub request_id: String,
    started_ms: u64,
    started: Instant,
    pub request_headers: Map<String, Value>,
    /// `prepare` before the request is sent, `stream` after, `connect` in backoff.
    pub phase: &'static str,
    pub first_event: Option<Instant>,
}

impl Attempt {
    pub fn new(number: u32) -> Self {
        Self {
            number,
            request_id: uuid::Uuid::new_v4().to_string(),
            started_ms: now_ms(),
            started: Instant::now(),
            request_headers: Map::new(),
            phase: "prepare",
            first_event: None,
        }
    }

    fn since(&self, at: Option<Instant>) -> Option<u64> {
        at.map(|at| at.saturating_duration_since(self.started).as_millis() as u64)
    }
}

/// Builds the status payloads of one logical request.
pub(crate) struct Reporter<'a> {
    pub config: &'a ModelConfig,
    pub origin: &'a RequestOrigin,
    pub max_attempts: u32,
}

impl Reporter<'_> {
    fn base(&self, kind: &str, attempt: &Attempt, at_ms: u64) -> Value {
        let provider_kind = match self.config.api_type {
            ApiType::Anthropic => "anthropic",
            ApiType::Responses => "openai",
            _ => "openai-compatible",
        };
        let mut status = json!({"type": kind, "timestamp": iso(at_ms), "traceId": self.origin.trace_id,
            "requestId": attempt.request_id, "providerId": self.config.provider_id,
            "modelId": self.config.model_id, "baseURL": self.config.base_url,
            "providerKind": provider_kind, "transport": "sse", "attempt": attempt.number,
            "maxAttempts": self.max_attempts});
        if let Some(query) = &self.origin.query_id {
            status["queryId"] = query.clone().into();
        }
        if let Some(session) = &self.origin.session_id {
            status["sessionId"] = session.clone().into();
        }
        if !self.origin.query_source.is_empty() {
            status["querySource"] = self.origin.query_source.into();
        }
        if attempt.phase != "prepare" {
            status["requestHeaderCount"] = attempt.request_headers.len().into();
            status["requestHeaders"] = attempt.request_headers.clone().into();
        } else {
            status["requestHeaderCount"] = 0.into();
            status["requestHeaders"] = json!({});
        }
        status
    }

    pub fn started(&self, attempt: &Attempt) -> Value {
        self.base("model_request_started", attempt, attempt.started_ms)
    }

    pub fn stalled(&self, attempt: &Attempt, timeout_ms: u64) -> Value {
        let mut status = self.base("model_stream_stalled", attempt, now_ms());
        status["idleMs"] = timeout_ms.into();
        status["timeoutMs"] = timeout_ms.into();
        status["message"] = stall_message(timeout_ms).into();
        status
    }

    /// `output` is `None` for a terminal empty completion (Node reports it as completed).
    pub fn completed(
        &self,
        attempt: &Attempt,
        output: Option<&ModelOutput>,
        response: &Map<String, Value>,
        timings: (Option<Instant>, Option<Instant>),
        committed: bool,
    ) -> Value {
        let mut status = self.base("model_request_completed", attempt, now_ms());
        status["durationMs"] = (attempt.started.elapsed().as_millis() as u64).into();
        let finish = match output {
            Some(o) if o.output_limit => "length",
            Some(o) if !o.calls.is_empty() => "tool-calls",
            _ => "stop",
        };
        status["finishReason"] = finish.into();
        status["usage"] = usage(output.map_or(&Value::Null, |o| &o.usage));
        if let Some(id) = PROVIDER_REQUEST_HEADERS.iter().find_map(|h| {
            response
                .get(*h)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
        }) {
            status["providerRequestId"] = id.chars().take(256).collect::<String>().into();
        }
        for (key, at) in [
            ("timeToFirstProviderEventMs", attempt.first_event),
            ("timeToFirstContentMs", timings.0),
            ("timeToFirstTextMs", timings.1),
        ] {
            if let Some(ms) = attempt.since(at) {
                status[key] = ms.into();
            }
        }
        status["streamStallCount"] = 0.into();
        status["streamOutputCommitted"] = committed.into();
        status["responseHeaderCount"] = response.len().into();
        status["responseHeaders"] = response.clone().into();
        status
    }

    /// `retryable` is the actual retry decision, not the classifier's flag.
    pub fn failed(
        &self,
        attempt: &Attempt,
        failure: &ModelFailure,
        retryable: bool,
        timeout_ms: u64,
    ) -> Value {
        let mut status = self.failure("model_request_failed", attempt, failure, timeout_ms);
        status["durationMs"] = (attempt.started.elapsed().as_millis() as u64).into();
        status["reason"] = node_reason(failure).into();
        status["retryable"] = retryable.into();
        status["errorPhase"] = attempt.phase.into();
        status["exceptionType"] = exception_type(failure).into();
        status["streamOutputCommitted"] = failure.output_committed.into();
        if let Some(detail) = &failure.detail {
            for (key, value) in [
                ("providerErrorCode", &detail.provider_error_code),
                ("providerErrorMessage", &detail.provider_error_message),
                ("providerRequestId", &detail.provider_request_id),
            ] {
                if let Some(value) = value {
                    status[key] = value.clone().into();
                }
            }
        }
        status
    }

    pub fn retry(
        &self,
        attempt: &Attempt,
        failure: &ModelFailure,
        delay_ms: u64,
        timeout_ms: u64,
    ) -> Value {
        let mut status = self.failure("model_retry_scheduled", attempt, failure, timeout_ms);
        status["delayMs"] = delay_ms.into();
        status["nextAttempt"] = (attempt.number + 1).into();
        let reason = if failure.empty_completion {
            "server_error"
        } else {
            node_reason(failure)
        };
        status["reason"] = reason.into();
        status
    }

    /// Fields `failed` and `retry_scheduled` share.
    fn failure(
        &self,
        kind: &str,
        attempt: &Attempt,
        failure: &ModelFailure,
        timeout_ms: u64,
    ) -> Value {
        let mut status = self.base(kind, attempt, now_ms());
        status["message"] = node_message(failure, timeout_ms).into();
        status["errorCode"] = failure.code.into();
        if let Some(code) = failure.status_code {
            status["statusCode"] = code.into();
        }
        if let Some(after) = failure.retry_after_ms {
            status["retryAfterMs"] = after.into();
        }
        let headers = failure.detail.as_ref().map(|d| d.response_headers.clone());
        let headers = headers.unwrap_or_default();
        status["responseHeaderCount"] = headers.len().into();
        status["responseHeaders"] = headers.into();
        status
    }
}

fn stall_message(timeout_ms: u64) -> String {
    format!("Model stream stalled: no event received for {timeout_ms}ms.")
}

/// Node `ModelFailureReason` for a Rust failure.
fn node_reason(failure: &ModelFailure) -> &'static str {
    match failure.reason {
        _ if failure.empty_completion => "unknown",
        "invalid_response" => "invalid_request",
        reason => reason,
    }
}

/// Node's message: the provider's own text for business errors, else the
/// classifier constant.
fn node_message(failure: &ModelFailure, timeout_ms: u64) -> String {
    if failure.empty_completion {
        return "Model returned no text, no tool calls, and no usage before completing the turn."
            .into();
    }
    if let Some(message) = failure
        .detail
        .as_ref()
        .and_then(|d| d.provider_error_message.clone())
    {
        return message;
    }
    match failure.reason {
        "stream_idle_timeout" => stall_message(timeout_ms),
        "context_exceeded" => "Model request exceeded the provider context window.".into(),
        "tls_error" => "TLS validation failed for the provider request.".into(),
        "network_error" => "Network connection failed for the provider request.".into(),
        _ => failure.message.into(),
    }
}

fn exception_type(failure: &ModelFailure) -> &'static str {
    match failure.reason {
        "stream_idle_timeout" => "ModelStreamIdleTimeoutError",
        "cancelled" => "AbortError",
        _ if failure
            .detail
            .as_ref()
            .is_some_and(|d| d.provider_error_code.is_some()) =>
        {
            "ProviderBusinessError"
        }
        _ => "AI_APICallError",
    }
}

/// Node `ModelUsage` from an OpenAI-style usage object; unknown fields are omitted.
fn usage(usage: &Value) -> Value {
    let mut out = Map::new();
    let input = usage["prompt_tokens"].as_u64();
    let output = usage["completion_tokens"].as_u64();
    let total = usage["total_tokens"]
        .as_u64()
        .or(input.zip(output).map(|(i, o)| i + o));
    for (key, value) in [
        ("inputTokens", input),
        ("outputTokens", output),
        ("totalTokens", total),
        (
            "cacheReadTokens",
            usage["prompt_tokens_details"]["cached_tokens"].as_u64(),
        ),
        (
            "cacheWriteTokens",
            usage["prompt_tokens_details"]["cache_write_tokens"].as_u64(),
        ),
        (
            "reasoningTokens",
            usage["completion_tokens_details"]["reasoning_tokens"].as_u64(),
        ),
    ] {
        if let Some(value) = value {
            out.insert(key.into(), value.into());
        }
    }
    out.into()
}

#[cfg(test)]
#[path = "network_status_tests.rs"]
mod tests;
