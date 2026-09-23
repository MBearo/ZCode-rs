use crate::contract::ModelFailure;
use reqwest::header::HeaderMap;
use serde_json::Value;
use std::{error::Error, time::SystemTime};

pub fn network(error: &reqwest::Error) -> ModelFailure {
    if is_tls_failure(error) {
        return ModelFailure::new("tls_error", false);
    }
    if error.is_builder() {
        ModelFailure::new("invalid_request", false)
    } else if error.is_timeout() {
        ModelFailure::new("timeout", true)
    } else {
        ModelFailure::new("network_error", true)
    }
}
fn is_tls_failure(error: &(dyn Error + 'static)) -> bool {
    let mut source = Some(error);
    while let Some(cause) = source {
        if cause.downcast_ref::<rustls::Error>().is_some() {
            return true;
        }
        // hyper-rustls 将握手错误包成两层 io::Error；source() 会跳过内层，必须展开 get_ref。
        if let Some(inner) = cause
            .downcast_ref::<std::io::Error>()
            .and_then(|e| e.get_ref())
        {
            source = Some(inner);
        } else {
            source = cause.source();
        }
    }
    false
}
pub fn response(status: Option<u16>, body: &Value, headers: &HeaderMap) -> ModelFailure {
    let error = body.get("error").filter(|v| !v.is_null()).unwrap_or(body);
    let code = error
        .get("code")
        .or_else(|| error.get("error_code"))
        .or_else(|| error.get("type"));
    let code = match code {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    };
    let status = status.filter(|n| (400..=599).contains(n)).or_else(|| {
        ["status", "statusCode", "responseStatus"]
            .iter()
            .find_map(|key| {
                error[*key]
                    .as_u64()
                    .and_then(|n| u16::try_from(n).ok())
                    .or_else(|| error[*key].as_str().and_then(|s| s.parse::<u16>().ok()))
                    .filter(|n| (400..=599).contains(n))
            })
    });
    let mut failure = match code.as_str() {
        "500" | "1120" | "1230" | "2007" => ModelFailure::new("server_error", true),
        "1006" => ModelFailure::new("auth_failed", false),
        "1005" => with_code("invalid_request", "model_request_failed"),
        "3006" => with_code("invalid_request", "model_not_found"),
        "3001" => ModelFailure::new("invalid_request", false),
        "3007" => with_code("auth_failed", "invalid_model_request"),
        "1234" => ModelFailure::new("network_error", true),
        "1261" | "context_length_exceeded" => ModelFailure::new("context_exceeded", false),
        "3008"
        | "3009"
        | "3010"
        | "1304"
        | "1308"
        | "1310"
        | "1313"
        | "insufficient_quota"
        | "credit_balance_exhausted"
        | "organization_spend_limit_exceeded"
        | "project_spend_limit_exceeded"
        | "organization_usage_limit_exceeded"
        | "exceeded_current_quota_error"
        | "2056"
        | "20097"
        | "1316"
        | "1317"
        | "1318"
        | "1319"
        | "1320"
        | "1321" => ModelFailure::new("rate_limited", false),
        "1302" | "1303" | "1305" | "3002" | "rate_limit_reached_error" | "rate_limit_error" => {
            ModelFailure::new("rate_limited", true)
        }
        "1312" | "engine_overloaded_error" | "overloaded_error" => {
            ModelFailure::new("provider_overloaded", true)
        }
        "1008" | "1113" | "1309" | "1311" | "1314" | "1315" => ModelFailure::new("unknown", false),
        _ => fallback(status, &code, error),
    };

    failure.status_code = status;
    failure.retry_after_ms = retry_after(headers, SystemTime::now());
    failure
}
fn fallback(status: Option<u16>, code: &str, error: &Value) -> ModelFailure {
    let lower = code.to_ascii_lowercase();
    let message = error["message"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "context_length_exceeded"
            | "context_window_exceeded"
            | "model_context_exceeded"
            | "model_context_window_exceeded"
    ) || (message.contains("context") && message.contains("exceed"))
        || (message.contains("maximum context length")
            && message.contains("tokens")
            && (message.contains("requested") || message.contains("resulted")))
        || message.contains("prompt is too long")
        || (message.contains("input token count")
            && message.contains("exceed")
            && message.contains("maximum number of tokens allowed"))
        || message.contains("range of input length should be")
        || (message.contains("total message token length")
            && message.contains("exceed")
            && message.contains("model limit"))
    {
        return ModelFailure::new("context_exceeded", false);
    }
    let upper = code.to_ascii_uppercase();
    if status == Some(408)
        || matches!(
            upper.as_str(),
            "ETIMEDOUT"
                | "ETIMEOUT"
                | "UND_ERR_CONNECT_TIMEOUT"
                | "UND_ERR_HEADERS_TIMEOUT"
                | "UND_ERR_BODY_TIMEOUT"
        )
    {
        return ModelFailure::new("timeout", true);
    }
    match status {
        Some(401 | 403) => return ModelFailure::new("auth_failed", false),
        Some(404) => return with_code("invalid_request", "model_not_found"),
        Some(400 | 422) => return ModelFailure::new("invalid_request", false),
        Some(429) => return ModelFailure::new("rate_limited", true),
        Some(529) => return ModelFailure::new("provider_overloaded", true),
        _ => {}
    }
    if matches!(
        upper.as_str(),
        "EPIPE"
            | "ECONNRESET"
            | "ECONNREFUSED"
            | "ENOTFOUND"
            | "EAI_AGAIN"
            | "ENETUNREACH"
            | "EHOSTUNREACH"
            | "UND_ERR_SOCKET"
    ) || matches!(lower.as_str(), "network_error" | "network_error_retryable")
        || (error["type"] == "api_error"
            && matches!(
                message.as_str(),
                "500 internal network error"
                    | "internal network error"
                    | "internal network failure"
            ))
    {
        return ModelFailure::new("network_error", true);
    }
    if status.is_some_and(|s| s >= 500) {
        ModelFailure::new("server_error", true)
    } else {
        ModelFailure::new("unknown", false)
    }
}

fn with_code(reason: &'static str, code: &'static str) -> ModelFailure {
    ModelFailure {
        code,
        ..ModelFailure::new(reason, false)
    }
}
fn retry_after(headers: &HeaderMap, now: SystemTime) -> Option<u64> {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
    };
    if header("x-should-retry").is_some_and(|s| s.eq_ignore_ascii_case("false") || s == "0") {
        return None;
    }
    let numeric = |s: &str, scale: f64| {
        s.parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
            .map(|v| (v * scale).max(0.0).round() as u64)
    };
    if let Some(ms) = header("retry-after-ms").and_then(|s| numeric(s, 1.0)) {
        return Some(ms);
    }
    let value = header("retry-after")?;
    numeric(value, 1000.0).or_else(|| {
        httpdate::parse_http_date(value)
            .ok()
            .map(|date| date.duration_since(now).unwrap_or_default().as_millis() as u64)
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retry_after_header_forms_and_precedence() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "retry-after",
            "Wed, 21 Oct 2015 07:28:00 GMT".parse().unwrap(),
        );
        let now = httpdate::parse_http_date("Wed, 21 Oct 2015 07:27:00 GMT").unwrap();
        assert_eq!(retry_after(&headers, now), Some(60000));
        headers.insert("retry-after-ms", "12.5".parse().unwrap());
        assert_eq!(retry_after(&headers, now), Some(13));
        headers.insert("x-should-retry", "false".parse().unwrap());
        assert_eq!(retry_after(&headers, now), None);
    }
}
