use super::*;
use serde_json::json;

fn who(compact: bool) -> Attribution {
    Attribution {
        session_id: "s".into(),
        run_id: "r".into(),
        turn_id: "t".into(),
        trace_id: "trace".into(),
        variant: Some("high".into()),
        mode: "build".into(),
        subagent: false,
        compact,
    }
}

fn status(kind: &str, source: &str, extra: serde_json::Value) -> serde_json::Value {
    let mut status = json!({"type": kind, "querySource": source, "attempt": 1,
        "providerId": "p", "modelId": "m"});
    for (key, value) in extra.as_object().unwrap() {
        status[key] = value.clone();
    }
    status
}

fn model(fact: &Fact) -> &ModelFact {
    match fact {
        Fact::Model(fact) => fact,
        other => panic!("not a model fact: {other:?}"),
    }
}

#[test]
fn agent_steps_record_at_model_done_with_retries_and_first_token() {
    let mut run = RunUsage::new(who(false), 1_000);
    assert!(
        run.on_status(
            &status("model_request_started", "main_turn", json!({})),
            1_010
        )
        .is_empty()
    );
    run.on_status(
        &status("model_retry_scheduled", "main_turn", json!({})),
        1_020,
    );
    run.on_status(
        &status("model_request_started", "main_turn", json!({"attempt": 2})),
        1_030,
    );
    run.on_text(1_050);
    run.on_text(1_060);
    let usage =
        json!({"inputTokens": 100, "outputTokens": 20, "totalTokens": 120, "cacheReadTokens": 60});
    let done = status(
        "model_request_completed",
        "main_turn",
        json!({"usage": usage, "finishReason": "tool-calls"}),
    );
    assert!(
        run.on_status(&done, 1_100).is_empty(),
        "steps wait for ModelDone"
    );
    let message = json!({"tool_calls": [{"id": "a"}, {"id": "b"}]});
    let facts = run.on_model_done(Some(&message));
    let fact = model(&facts[0]);
    assert_eq!(fact.id, "usage_model_main_turn_r:1_0");
    assert_eq!(
        (fact.status, fact.retry_count, fact.retryable),
        ("completed", 1, true)
    );
    assert_eq!(
        (fact.started_at, fact.first_token_at, fact.completed_at),
        (1_010, Some(1_050), 1_100)
    );
    assert_eq!(
        (
            fact.tool_call_count,
            fact.tokens.input,
            fact.tokens.cache_read
        ),
        (2, 100, 60)
    );
    assert_eq!(
        (fact.tokens.computed_total(), fact.provider_total_tokens),
        (120, Some(120))
    );
    assert_eq!(fact.finish_reason.as_deref(), Some("tool-calls"));
    assert_eq!(fact.task_type, "interactive");
    let turn = run.finish(Outcome::Completed, None, 1_200);
    let Fact::Turn(turn) = &turn[0] else {
        panic!("turn fact")
    };
    assert_eq!(
        (turn.status, turn.started_at, turn.completed_at),
        ("completed", 1_000, 1_200)
    );
    assert_eq!((turn.model_request_count, turn.model_retry_count), (1, 1));
    assert_eq!(
        (turn.first_model_start_at, turn.first_token_at),
        (Some(1_010), Some(1_050))
    );
    assert_eq!((turn.tokens.input, turn.computed_total_tokens), (100, 120));
}

#[test]
fn other_sources_record_at_their_end_and_tool_internal_requests_are_skipped() {
    let mut run = RunUsage::new(who(false), 0);
    assert!(
        run.on_status(
            &status("model_request_started", "web_fetch_processing", json!({})),
            1
        )
        .is_empty()
    );
    assert!(
        run.on_status(
            &status("model_request_completed", "web_fetch_processing", json!({})),
            2
        )
        .is_empty()
    );
    run.on_status(&status("model_request_started", "compact", json!({})), 3);
    let facts = run.on_status(
        &status("model_request_completed", "compact", json!({"usage": {}})),
        4,
    );
    assert_eq!(
        (
            model(&facts[0]).query_source.as_str(),
            model(&facts[0]).tool_call_count
        ),
        ("compact", 0)
    );
    run.on_status(
        &status(
            "model_request_started",
            "target_completion_verification",
            json!({}),
        ),
        5,
    );
    let failed = json!({"retryable": false, "reason": "context_exceeded", "errorCode": "E", "message": "too long"});
    let facts = run.on_status(
        &status(
            "model_request_failed",
            "target_completion_verification",
            failed,
        ),
        6,
    );
    let fact = model(&facts[0]);
    assert_eq!((fact.status, fact.context_exceeded), ("error", true));
    assert_eq!(fact.error.kind.as_deref(), Some("context_exceeded"));
    run.on_status(&status("model_request_started", "main_turn", json!({})), 7);
    let retrying = json!({"retryable": true, "reason": "rate_limit"});
    assert!(
        run.on_status(&status("model_request_failed", "main_turn", retrying), 8)
            .is_empty()
    );
    let cancelled = json!({"retryable": false, "reason": "cancelled"});
    let facts = run.on_status(&status("model_request_failed", "main_turn", cancelled), 9);
    assert_eq!(model(&facts[0]).status, "cancelled");
}

#[test]
fn tools_record_running_then_their_end_and_open_ones_close_with_the_run() {
    let mut run = RunUsage::new(who(false), 0);
    let call = |id: &str| json!({"id": id, "function": {"name": "Bash"}});
    let Fact::Tool(running) = &run.on_tool_start(&call("a"), 10)[0] else {
        panic!()
    };
    assert_eq!(
        (running.status, running.approval_status, running.id()),
        ("running", "none", "usage_tool_s_a".into())
    );
    run.on_permission("a");
    run.on_tool_executing("a", 15);
    let Fact::Tool(done) = &run.on_tool_done("a", (false, false, false), "ok", 40)[0] else {
        panic!()
    };
    assert_eq!(
        (
            done.status,
            done.approval_status,
            done.duration_ms,
            done.output_bytes
        ),
        ("completed", "allowed", Some(25), 2)
    );
    run.on_tool_start(&call("b"), 50);
    let Fact::Tool(denied) = &run.on_tool_done("b", (false, true, true), "no", 60)[0] else {
        panic!()
    };
    assert_eq!(
        (denied.status, denied.approval_status, denied.duration_ms),
        ("error", "denied", None)
    );
    run.on_tool_start(&call("c"), 70);
    let facts = run.finish(Outcome::Cancelled, None, 80);
    let Fact::Tool(open) = &facts[0] else {
        panic!()
    };
    assert_eq!((open.status, open.cancelled_by_user), ("cancelled", true));
    assert_eq!(
        facts.len(),
        1,
        "a cancelled prompt turn is not recorded (Node turn.ts)"
    );
}

#[test]
fn manual_compaction_records_its_turn_in_every_outcome() {
    let mut run = RunUsage::new(who(true), 0);
    run.on_status(&status("model_request_started", "compact", json!({})), 1);
    let facts = run.finish(Outcome::Failed, Some(("server_error", "E500")), 5);
    assert_eq!(model(&facts[0]).status, "error");
    let Fact::Turn(turn) = &facts[1] else {
        panic!()
    };
    assert_eq!(
        (
            turn.status,
            turn.error.kind.as_deref(),
            turn.error.code.as_deref()
        ),
        ("error", Some("server_error"), Some("E500"))
    );
}
