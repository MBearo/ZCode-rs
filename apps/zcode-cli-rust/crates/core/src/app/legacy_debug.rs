//! Model network status facts of a root session's run: the `session/debug`
//! observation and the legacy `session.updated` (Node `observeSessionDebug`,
//! `mapModelNetworkStatusPayload`, `shouldHideProtocolSessionEvent`).
use super::Engine;
use crate::{contract::RuntimeError, domain::legacy_params};
use anyhow::Result;
use serde_json::{Value, json};

impl Engine {
    /// Observed for `session/debug` whether or not a legacy stream is
    /// subscribed; title requests stay out of the stream.
    pub(super) fn model_status(&mut self, id: &str, turn: &str, mut status: Value) {
        let now = self.clock.now();
        let event_id = self.clock.id();
        let Some(s) = self.sessions.get_mut(id).filter(|s| s.parent_id.is_none()) else {
            return;
        };
        status["turnId"] = turn.into();
        let trace = s.runtime_trace.clone().unwrap_or_default();
        s.runtime.debug.observe(&status, &event_id, &trace, now);
        if status["querySource"] == "session_title" {
            return;
        }
        if let Some(retry) = api_retry(&status) {
            status["_meta"] = json!({"zcode": {"apiRetry": retry}});
        }
        self.legacy_emit(id, Some(turn), vec![("session.updated", status)]);
    }

    /// `session/debug`: Node parses with `schema.parse`, so invalid params are a
    /// plain `ZodError` (-32603), not the protocol's -32602.
    pub(super) fn session_debug(&self, raw: &Value) -> Result<Value> {
        let p = legacy_params::debug(raw).map_err(|error| RuntimeError::Named {
            name: "ZodError",
            message: error.data["message"].as_str().unwrap_or_default().into(),
            code: None,
        })?;
        let id = p["sessionId"].as_str().unwrap_or_default();
        let Some(s) = self.sessions.get(id) else {
            return Err(RuntimeError::Coded {
                code: -32004,
                message: format!("Session is not active: {id}"),
            }
            .into());
        };
        Ok(s.runtime.debug.snapshot(id))
    }
}

/// Node `zcodeApiRetryFromModelNetworkStatusPayload`; `None` adds no meta and
/// `Some(null)` clears the Host's retry indicator.
fn api_retry(status: &Value) -> Option<Value> {
    match status["type"].as_str() {
        Some("model_retry_scheduled") => Some(retry_status(status)),
        Some("model_request_started") => {
            // 恢复请求在适配层是 attempt 1，重试次数取 streamRecovery（Node 同样处理）。
            if let Some(retry) =
                crate::domain::stream_recovery::legacy_retry(&status["streamRecovery"])
            {
                return Some(retry);
            }
            (status["attempt"].as_u64().unwrap_or(1) <= 1).then_some(Value::Null)
        }
        Some("model_request_completed") => Some(Value::Null),
        Some("model_request_failed") if status["retryable"] != true => Some(Value::Null),
        _ => None,
    }
}

/// Node `normalizeZCodeApiRetryStatus`.
fn retry_status(record: &Value) -> Value {
    let positive = |key: &str| record[key].as_u64().filter(|n| *n > 0);
    let count = |key: &str| record[key].as_u64();
    let text = |key: &str| record[key].as_str().filter(|s| !s.is_empty());
    let attempt = positive("attempt").unwrap_or_else(|| {
        positive("nextAttempt")
            .unwrap_or(2)
            .saturating_sub(1)
            .max(1)
    });
    let max_retries = count("maxRetries")
        .unwrap_or_else(|| positive("maxAttempts").unwrap_or(attempt + 1) - 1)
        .max(attempt);
    json!({"kind": "api_retry", "attempt": attempt, "maxRetries": max_retries,
        "retryDelayMs": count("retryDelayMs").or(count("delayMs")).unwrap_or(0),
        "errorStatus": count("errorStatus").or(count("statusCode")),
        "error": text("error").or(text("message")).or(text("reason")).unwrap_or("Model retry scheduled")})
}
