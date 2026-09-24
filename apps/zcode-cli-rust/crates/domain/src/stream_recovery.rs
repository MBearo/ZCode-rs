//! Retry state and stream recovery rules (Node `streaming-recovery.ts`,
//! `modelRetryReasonCode` and `zcode-api-retry-status.ts`). Spec rust-m7-stream-recovery.
use super::model::{ModelFailure, RetryState};
use regex::Regex;
use serde_json::{Value, json};
use std::sync::OnceLock;

/// Node `STREAM_RECOVERY_MAX_RETRIES`, per turn.
pub const MAX_RETRIES: u32 = 10;
const TRANSIENT_REASONS: [&str; 5] = [
    "stream_idle_timeout",
    "rate_limited",
    "server_error",
    "network_error",
    "timeout",
];

const START_PLAN_PROVIDERS: [&str; 2] = ["account:bigmodel-start-plan", "account:zai-start-plan"];
const BUSY_CODES: [&str; 3] = ["3008", "3009", "3010"];
/// Node `START_PLAN_BUSY_MAIN_TURN_ADMISSION_RETRY_DELAYS_MS`.
const BUSY_DELAYS_MS: [u64; 2] = [1_000, 2_000];
pub const BUSY_MAX_RETRIES: u32 = BUSY_DELAYS_MS.len() as u32;
const BUSY_EXHAUSTED: &str =
    "Start Plan is busy and automatic model stream recovery reached the maximum retry count.";

/// Node `isStartPlanBusyStreamRecoveryFailure`: provider code 3008–3010.
pub fn start_plan_busy(failure: &ModelFailure) -> bool {
    failure
        .detail
        .as_ref()
        .and_then(|d| d.provider_error_code.as_deref())
        .is_some_and(|code| BUSY_CODES.contains(&code))
}

/// Node `getStartPlanBusyAdmissionRetryDelayMs`: the wait before retrying a
/// busy Start Plan request of a returning session (`used`: recoveries so far).
pub fn busy_delay(
    failure: &ModelFailure,
    provider: &str,
    returning: bool,
    used: u32,
) -> Option<u64> {
    if !returning || !START_PLAN_PROVIDERS.contains(&provider) || !start_plan_busy(failure) {
        return None;
    }
    BUSY_DELAYS_MS.get(used as usize).copied()
}

/// Node `createStartPlanBusyAutoRetryExhaustedError`.
pub fn busy_exhausted(failure: &ModelFailure) -> ModelFailure {
    ModelFailure {
        code: "model_rate_limited",
        reason: "rate_limited",
        message: BUSY_EXHAUSTED,
        retryable: false,
        ..failure.clone()
    }
}

/// Node `modelRetryReasonCode`: a retry reason as the V4 `apiRetry.reasonCode`.
pub fn retry_reason_code(reason: &str) -> &'static str {
    match reason {
        "rate_limited" | "offpeak_queued" => "fault.provider.rateLimited",
        "provider_overloaded" | "server_error" => "fault.provider.serverError",
        "timeout" => "fault.network.timeout",
        "stream_idle_timeout" => "fault.network.sseStalled",
        "stale_connection" => "fault.network.sseDisconnected",
        "network_error" => "fault.network.unreachable",
        _ => "fault.provider.requestFailed",
    }
}

fn pattern(cell: &'static OnceLock<Regex>, source: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(source).expect("valid pattern"))
}

/// Node `classifyStreamRecoveryFailure` from the failed status's reason and message.
pub fn failure_kind(reason: &str, message: &str) -> &'static str {
    static TIMEOUT: OnceLock<Regex> = OnceLock::new();
    static NETWORK: OnceLock<Regex> = OnceLock::new();
    let timeout = pattern(&TIMEOUT, r"(?i)\btimeout|timed out|stalled\b");
    let network = pattern(&NETWORK, r"(?i)\bECONNRESET|EPIPE|ETIMEDOUT\b");
    if matches!(reason, "stream_idle_timeout" | "timeout") || timeout.is_match(message) {
        "provider_timeout"
    } else if reason == "network_error" || network.is_match(message) {
        "provider_network_error"
    } else {
        "provider_stream_error"
    }
}

/// Node `streamRecoveryReasonCode`.
pub fn kind_reason_code(kind: &str) -> &'static str {
    match kind {
        "provider_timeout" => "fault.network.timeout",
        "provider_network_error" => "fault.network.unreachable",
        "provider_stream_error" => "fault.network.sseDisconnected",
        _ => "fault.provider.requestFailed",
    }
}

/// Whether an agent step failure is recovered with a new request (`used`:
/// recoveries so far in the run).
pub fn recoverable(failure: &ModelFailure, used: u32) -> bool {
    failure.output_committed
        && used < MAX_RETRIES
        && (failure.retryable || TRANSIENT_REASONS.contains(&failure.reason))
}

/// The retry state while a recovery request is pending.
pub fn recovery_state(retry: u32, max: u32, now: u64, reason_code: &'static str) -> RetryState {
    RetryState {
        attempt: retry.max(1),
        max_attempts: max.max(retry) + 1,
        next_retry_at: now,
        reason_code,
    }
}

/// Node V4 `onModelNetworkStatus`: the retry state after one network status;
/// `None` leaves it unchanged, `Some(None)` clears it.
pub fn status_retry(
    status: &Value,
    current: Option<&RetryState>,
    now: u64,
) -> Option<Option<RetryState>> {
    let attempt = status["attempt"].as_u64().filter(|n| *n > 0).unwrap_or(1) as u32;
    match status["type"].as_str().unwrap_or("") {
        "model_retry_scheduled" => {
            let max = status["maxAttempts"].as_u64().filter(|n| *n > 0);
            let max = (max.unwrap_or(u64::from(attempt) + 1) as u32).max(attempt + 1);
            Some(Some(RetryState {
                attempt,
                max_attempts: max,
                next_retry_at: now + status["delayMs"].as_u64().unwrap_or(0),
                reason_code: retry_reason_code(status["reason"].as_str().unwrap_or("")),
            }))
        }
        "model_request_started" => match status.get("streamRecovery").filter(|r| r.is_object()) {
            Some(recovery) => {
                let retry = recovery["retryNumber"].as_u64().unwrap_or(1) as u32;
                let max = recovery["maxRetries"].as_u64().unwrap_or(0) as u32;
                let reason = current.map_or("fault.network.sseDisconnected", |c| c.reason_code);
                Some(Some(recovery_state(retry, max, now, reason)))
            }
            // attempt ≥ 2 只说明重试已发出，等首个有效进展再清理（Node 同样处理）。
            None => (attempt <= 1).then_some(None),
        },
        "model_request_completed" => Some(None),
        "model_request_failed" if status["retryable"] != true => Some(None),
        _ => None,
    }
}

/// Node `zcodeApiRetryFromStreamRecoveryPayload` for a legacy payload.
pub fn legacy_retry(record: &Value) -> Option<Value> {
    let attempt = record["retryNumber"].as_u64().filter(|n| *n > 0)?;
    let max = record["maxRetries"]
        .as_u64()
        .unwrap_or(attempt)
        .max(attempt);
    let error = record["message"]
        .as_str()
        .unwrap_or("Model stream recovery retry started");
    Some(
        json!({"kind": "api_retry", "attempt": attempt, "maxRetries": max,
        "retryDelayMs": 0, "errorStatus": null, "error": error}),
    )
}

/// What the owner saw of the current agent step's request, for the recovery facts.
#[derive(Clone, Debug, Default)]
pub struct StepProbe {
    /// The latest `model_request_started` request id.
    pub started: Option<String>,
    /// The latest failed or stalled request: `(requestId, reason, message)`.
    pub failed: Option<(String, String, String)>,
    /// The request's model `(providerId, modelId)`.
    pub model: Option<(String, String)>,
    /// The streamed response not yet committed, and its text / reasoning UTF-8 bytes.
    pub response: Option<String>,
    pub text_bytes: u64,
    pub reasoning_bytes: u64,
}

impl StepProbe {
    /// One network status of the run; statuses of other sources are ignored.
    pub fn observe_status(&mut self, status: &Value) {
        if !matches!(
            status["querySource"].as_str(),
            Some("main_turn" | "subagent")
        ) {
            return;
        }
        let request = status["requestId"].as_str().unwrap_or("").to_owned();
        match status["type"].as_str().unwrap_or("") {
            "model_request_started" => {
                if status["attempt"].as_u64().unwrap_or(1) <= 1 {
                    *self = Self::default();
                }
                self.started = Some(request);
                let text = |key: &str| status[key].as_str().unwrap_or("").to_owned();
                self.model = Some((text("providerId"), text("modelId")));
            }
            "model_request_failed" | "model_stream_stalled" => {
                let text = |key: &str| status[key].as_str().unwrap_or("").to_owned();
                self.failed = Some((request, text("reason"), text("message")));
            }
            _ => {}
        }
    }

    pub fn observe_text(&mut self, response: &str, text: &str, reasoning: bool) {
        self.response = Some(response.to_owned());
        if reasoning {
            self.reasoning_bytes += text.len() as u64;
        } else {
            self.text_bytes += text.len() as u64;
        }
    }
}

#[cfg(test)]
#[path = "stream_recovery_tests.rs"]
mod tests;
