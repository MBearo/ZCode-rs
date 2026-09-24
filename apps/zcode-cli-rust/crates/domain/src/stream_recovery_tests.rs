use super::*;

#[test]
fn reason_codes_follow_node() {
    assert_eq!(
        retry_reason_code("rate_limited"),
        "fault.provider.rateLimited"
    );
    assert_eq!(
        retry_reason_code("server_error"),
        "fault.provider.serverError"
    );
    assert_eq!(
        retry_reason_code("stream_idle_timeout"),
        "fault.network.sseStalled"
    );
    assert_eq!(
        retry_reason_code("network_error"),
        "fault.network.unreachable"
    );
    assert_eq!(
        retry_reason_code("auth_refresh"),
        "fault.provider.requestFailed"
    );
    assert_eq!(failure_kind("stream_idle_timeout", ""), "provider_timeout");
    assert_eq!(
        failure_kind("server_error", "Request timed out"),
        "provider_timeout"
    );
    assert_eq!(failure_kind("network_error", ""), "provider_network_error");
    assert_eq!(
        failure_kind("unknown", "read ECONNRESET"),
        "provider_network_error"
    );
    assert_eq!(
        failure_kind("server_error", "boom"),
        "provider_stream_error"
    );
    assert_eq!(
        kind_reason_code("provider_stream_error"),
        "fault.network.sseDisconnected"
    );
}

#[test]
fn recovery_needs_visible_output_a_transient_failure_and_budget() {
    let mut failure = ModelFailure::new("network_error", true);
    assert!(
        !recoverable(&failure, 0),
        "nothing visible: the adapter retries"
    );
    failure.output_committed = true;
    assert!(recoverable(&failure, 9));
    assert!(!recoverable(&failure, 10));
    let mut rejected = ModelFailure::new("auth_failed", false);
    rejected.output_committed = true;
    assert!(!recoverable(&rejected, 0));
    let mut limited = ModelFailure::new("rate_limited", false);
    limited.output_committed = true;
    assert!(
        recoverable(&limited, 0),
        "transient reasons recover even when not retryable"
    );
}

#[test]
fn the_step_probe_tracks_the_failed_request_and_streamed_bytes() {
    let status = |kind: &str, id: &str, attempt: u64| {
        json!({"type": kind, "querySource": "main_turn", "requestId": id, "attempt": attempt,
            "reason": "stream_idle_timeout", "message": "stalled"})
    };
    let mut probe = StepProbe::default();
    probe.observe_status(&status("model_request_started", "a", 1));
    probe.observe_text("r1", "hé", false);
    probe.observe_text("r1", "hm", true);
    probe.observe_status(&status("model_stream_stalled", "a", 1));
    probe.observe_status(&json!({"type": "model_request_started", "querySource": "compact"}));
    assert_eq!(probe.started.as_deref(), Some("a"));
    assert_eq!(probe.failed.as_ref().map(|f| f.0.as_str()), Some("a"));
    assert_eq!((probe.text_bytes, probe.reasoning_bytes), (3, 2));
    probe.observe_status(&status("model_request_started", "b", 1));
    assert_eq!((probe.failed.is_none(), probe.text_bytes), (true, 0));
    assert_eq!(
        legacy_retry(&json!({"retryNumber": 2, "maxRetries": 10})).unwrap()["error"],
        "Model stream recovery retry started"
    );
    assert_eq!(legacy_retry(&json!({"anchorId": "x"})), None);
}

#[test]
fn network_statuses_drive_the_retry_state_like_node() {
    let scheduled = json!({"type": "model_retry_scheduled", "attempt": 1, "maxAttempts": 3,
        "delayMs": 500, "reason": "rate_limited"});
    let state = status_retry(&scheduled, None, 1_000).unwrap().unwrap();
    assert_eq!(
        (
            state.attempt,
            state.max_attempts,
            state.next_retry_at,
            state.reason_code
        ),
        (1, 3, 1_500, "fault.provider.rateLimited")
    );
    let again = json!({"type": "model_request_started", "attempt": 2});
    assert_eq!(
        status_retry(&again, Some(&state), 2_000),
        None,
        "kept until progress"
    );
    let first = json!({"type": "model_request_started", "attempt": 1});
    assert_eq!(status_retry(&first, Some(&state), 2_000), Some(None));
    let recovered = json!({"type": "model_request_started", "attempt": 1,
        "streamRecovery": {"retryNumber": 2, "maxRetries": 10}});
    let recovering = status_retry(&recovered, Some(&state), 3_000)
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            recovering.attempt,
            recovering.max_attempts,
            recovering.reason_code
        ),
        (2, 11, "fault.provider.rateLimited")
    );
    let retrying = json!({"type": "model_request_failed", "retryable": true});
    assert_eq!(status_retry(&retrying, Some(&state), 0), None);
    let failed = json!({"type": "model_request_failed", "retryable": false});
    assert_eq!(status_retry(&failed, Some(&state), 0), Some(None));
}

#[test]
fn start_plan_busy_retries_follow_node() {
    let mut failure = ModelFailure::new("rate_limited", false);
    failure.detail = Some(Box::new(crate::model::FailureDetail {
        provider_error_code: Some("3009".into()),
        ..Default::default()
    }));
    let plan = "account:zai-start-plan";
    assert_eq!(busy_delay(&failure, plan, true, 0), Some(1_000));
    assert_eq!(busy_delay(&failure, plan, true, 1), Some(2_000));
    assert_eq!(busy_delay(&failure, plan, true, 2), None);
    assert_eq!(
        busy_delay(&failure, plan, false, 0),
        None,
        "not on the first turn"
    );
    assert_eq!(busy_delay(&failure, "fixture", true, 0), None);
    let exhausted = busy_exhausted(&failure);
    assert_eq!(
        (exhausted.code, exhausted.reason, exhausted.retryable),
        ("model_rate_limited", "rate_limited", false)
    );
    assert!(start_plan_busy(&exhausted), "the provider code stays");
    assert!(!start_plan_busy(&ModelFailure::new("rate_limited", false)));
}
