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
        agent: "zcode-agent".into(),
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

fn ids(assistant: Option<&str>) -> StepIds {
    StepIds {
        user: Some("msg_user".into()),
        assistant: assistant.map(str::to_owned),
    }
}

fn span() -> String {
    "span-0123456789a".into()
}

fn model(fact: &Fact) -> &ModelFact {
    match fact {
        Fact::Model(fact) => fact,
        other => panic!("not a model fact: {other:?}"),
    }
}

fn turn(facts: &[Fact]) -> &TurnFact {
    facts
        .iter()
        .find_map(|f| match f {
            Fact::Turn(turn) => Some(turn.as_ref()),
            _ => None,
        })
        .expect("a turn fact")
}

#[test]
fn agent_steps_record_at_model_done_with_node_ids() {
    let mut run = RunUsage::new(who(false), 1_000);
    let started = status("model_request_started", "main_turn", json!({}));
    assert!(run.on_status(&started, &ids(None), span, 1_010).is_empty());
    let retry = status("model_retry_scheduled", "main_turn", json!({}));
    run.on_status(&retry, &ids(None), span, 1_020);
    let again = status("model_request_started", "main_turn", json!({"attempt": 2}));
    run.on_status(&again, &ids(None), span, 1_030);
    run.on_text(1_050);
    run.on_text(1_060);
    let usage =
        json!({"inputTokens": 100, "outputTokens": 20, "totalTokens": 120, "cacheReadTokens": 60});
    let done = status(
        "model_request_completed",
        "main_turn",
        json!({"usage": usage, "finishReason": "tool-calls", RAW_FINISH_REASON: "tool_use"}),
    );
    let step = ids(Some("msg_assistant"));
    assert!(
        run.on_status(&done, &step, span, 1_100).is_empty(),
        "steps wait for ModelDone"
    );
    let message = json!({"tool_calls": [{"id": "a"}, {"id": "b"}]});
    let facts = run.on_model_done(Some(&message), &step);
    let fact = model(&facts[0]);
    // Node：step 以 assistant 消息为逻辑请求 id，并带 span、父 user 消息与原始结束原因。
    assert_eq!(fact.id, "usage_model_main_turn_msg_assistant_0");
    assert_eq!(fact.logical_request_id, "msg_assistant");
    assert_eq!(fact.span_id.as_deref(), Some("span-0123456789a"));
    assert_eq!(fact.parent_user_message_id.as_deref(), Some("msg_user"));
    assert_eq!(
        fact.provider_metadata,
        Some(json!({"rawFinishReason": "tool_use"}))
    );
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
    assert_eq!(fact.agent, "zcode-agent");
    let facts = run.finish(Outcome::Completed, None, Some("msg_user".into()), 1_200);
    let turn = turn(&facts);
    assert_eq!(
        (turn.status, turn.started_at, turn.completed_at),
        ("completed", 1_000, 1_200)
    );
    assert_eq!(turn.user_message_id.as_deref(), Some("msg_user"));
    assert_eq!((turn.model_request_count, turn.model_retry_count), (1, 1));
    assert_eq!((turn.tokens.input, turn.computed_total_tokens), (100, 120));
}

#[test]
fn failures_use_node_error_fields_and_other_sources_use_span_ids() {
    let mut run = RunUsage::new(who(false), 0);
    let fetch = status("model_request_started", "web_fetch_processing", json!({}));
    assert!(run.on_status(&fetch, &ids(None), span, 1).is_empty());
    run.on_status(
        &status("model_request_started", "compact", json!({})),
        &ids(Some("msg_a")),
        span,
        3,
    );
    let usage = json!({"inputTokens": 30, "outputTokens": 5});
    let facts = run.on_status(
        &status(
            "model_request_completed",
            "compact",
            json!({"usage": usage}),
        ),
        &ids(Some("msg_a")),
        span,
        4,
    );
    let compact = model(&facts[0]);
    // 压缩没有 assistant 与父 user 消息：逻辑请求 id 是 span id。
    assert_eq!(compact.id, "usage_model_compact_span-0123456789a_0");
    assert_eq!(
        (
            &compact.assistant_message_id,
            &compact.parent_user_message_id
        ),
        (&None, &None)
    );
    run.on_status(
        &status(
            "model_request_started",
            "target_completion_verification",
            json!({}),
        ),
        &ids(None),
        span,
        5,
    );
    let failed = json!({"retryable": false, "reason": "invalid_request", "errorCode": "E",
        "message": "provider text"});
    let facts = run.on_status(
        &status(
            "model_request_failed",
            "target_completion_verification",
            failed,
        ),
        &ids(None),
        span,
        6,
    );
    let fact = model(&facts[0]);
    // Node 适配器错误：reason 作类型，没有 code，message 是通用文案而非供应商原文。
    assert_eq!(fact.status, "error");
    assert_eq!(fact.error.kind.as_deref(), Some("invalid_request"));
    assert_eq!(fact.error.code, None);
    assert_eq!(
        fact.error.message.as_deref(),
        Some("Provider rejected the model request.")
    );
    run.on_status(
        &status("model_request_started", "main_turn", json!({})),
        &ids(None),
        span,
        7,
    );
    let cancelled = json!({"retryable": false, "reason": "cancelled"});
    let facts = run.on_status(
        &status("model_request_failed", "main_turn", cancelled),
        &ids(Some("msg_b")),
        span,
        9,
    );
    assert_eq!(model(&facts[0]).status, "cancelled");
    assert_eq!(
        model(&facts[0]).error.message.as_deref(),
        Some("Model request was cancelled.")
    );
    // 目标验证不在本轮事件里：请求数与 token 只算压缩与 agent step。
    let facts = run.finish(Outcome::Cancelled, None, Some("msg_user".into()), 10);
    let turn = turn(&facts);
    assert_eq!((turn.model_request_count, turn.tokens.input), (2, 30));
    assert_eq!(
        (turn.error.kind.as_deref(), turn.error.code.as_deref()),
        (Some("turn_cancelled"), Some("TURN_CANCELLED"))
    );
    assert!(turn.cancelled_by_user && !turn.retryable);
}

#[test]
fn every_outcome_records_the_turn_with_node_failure_kinds() {
    let mut run = RunUsage::new(who(false), 0);
    let facts = run.finish(Outcome::Failed, Some(("server_error", "E")), None, 5);
    let failed = turn(&facts);
    assert_eq!(
        (
            failed.status,
            failed.error.kind.as_deref(),
            failed.error.code.as_deref()
        ),
        ("error", Some("unknown_error"), Some("UNKNOWN_ERROR"))
    );
    let mut run = RunUsage::new(who(false), 0);
    let facts = run.finish(Outcome::Failed, Some(("context_exceeded", "E")), None, 5);
    let exceeded = turn(&facts);
    assert_eq!(
        exceeded.error.code.as_deref(),
        Some("MODEL_CONTEXT_EXCEEDED")
    );
    assert!(exceeded.retryable && exceeded.context_exceeded);
    let mut run = RunUsage::new(who(true), 0);
    let facts = run.finish(Outcome::Completed, None, Some("msg_user".into()), 5);
    assert_eq!(turn(&facts).user_message_id, None, "manual compaction");
}

#[test]
fn nested_tool_usage_joins_the_turn_only() {
    let mut run = RunUsage::new(who(false), 0);
    assert_eq!(run.total_tokens(), None);
    run.on_nested_usage(&json!({"inputTokens": 50, "outputTokens": 5, "totalTokens": 55}));
    let facts = run.finish(Outcome::Completed, None, None, 1);
    assert_eq!(facts.len(), 1, "no model_usage row for tool_internal usage");
    let turn = turn(&facts);
    assert_eq!((turn.tokens.input, turn.computed_total_tokens), (50, 55));
    assert_eq!(turn.model_request_count, 0);
    assert_eq!(run.total_tokens(), Some(55));
}

#[test]
fn tools_record_node_metadata_output_and_exit_codes() {
    let mut run = RunUsage::new(who(false), 0);
    let call = |id: &str| json!({"id": id, "function": {"name": "Bash"}});
    let meta = tool_meta("Bash", None);
    let Fact::Tool(running) = &run.on_tool_start(&call("a"), meta, 10)[0] else {
        panic!()
    };
    assert_eq!(
        (running.status, running.approval_status, running.id()),
        ("running", "none", "usage_tool_s_a".into())
    );
    assert_eq!(
        running.meta.map(|m| (m.scope, m.read_only)),
        Some(("system", false))
    );
    run.on_permission("a");
    run.on_tool_executing("a", 15);
    let end = ToolEnd {
        result: "ok",
        exit_code: Some(3),
        ..ToolEnd::default()
    };
    let Fact::Tool(done) = &run.on_tool_done("a", end, 40)[0] else {
        panic!()
    };
    assert_eq!(
        (
            done.status,
            done.approval_status,
            done.duration_ms,
            done.output_bytes
        ),
        ("completed", "none", Some(25), 2)
    );
    assert_eq!(
        (
            done.first_output_at,
            done.time_to_first_output_ms,
            done.exit_code
        ),
        (Some(40), Some(25), Some(3))
    );
    run.on_tool_start(&call("w"), tool_meta("WebSearch", None), 41);
    let end = ToolEnd {
        result: "found",
        ..ToolEnd::default()
    };
    let Fact::Tool(search) = &run.on_tool_done("w", end, 45)[0] else {
        panic!()
    };
    assert_eq!(
        search.exit_code,
        Some(0),
        "Node writes 0 without an exit code"
    );
    run.on_tool_start(&call("b"), meta, 50);
    let end = ToolEnd {
        denied: true,
        cancelled: true,
        result: "no",
        ..ToolEnd::default()
    };
    let Fact::Tool(denied) = &run.on_tool_done("b", end, 60)[0] else {
        panic!()
    };
    assert_eq!(
        (
            denied.status,
            denied.approval_status,
            denied.duration_ms,
            denied.exit_code
        ),
        ("error", "none", Some(10), None)
    );
    // Node：结果记录器最后写 approvalStatus "none"；拒绝没有输出字节，也没有 CoreError code。
    assert_eq!(denied.output_bytes, 0);
    assert_eq!(
        (denied.error.kind.as_deref(), denied.error.code.as_deref()),
        (Some("permission_denied"), None)
    );
    run.on_tool_start(&call("f"), meta, 61);
    let end = ToolEnd {
        failed: true,
        result: "boom",
        ..ToolEnd::default()
    };
    let Fact::Tool(failed) = &run.on_tool_done("f", end, 62)[0] else {
        panic!()
    };
    assert_eq!(failed.error.code.as_deref(), Some("TOOL_EXECUTION_FAILED"));
    run.on_tool_start(&call("c"), meta, 70);
    let facts = run.finish(Outcome::Cancelled, None, None, 80);
    let Fact::Tool(open) = &facts[0] else {
        panic!()
    };
    assert_eq!((open.status, open.cancelled_by_user), ("cancelled", true));
    assert_eq!(turn(&facts).tool_error_count, 1);
}

#[test]
fn tool_metadata_follows_the_node_registry() {
    assert_eq!(tool_meta("Read", None).map(|m| m.scope), Some("none"));
    assert_eq!(
        tool_meta("Agent", None).map(|m| (m.scope, m.read_only)),
        Some(("session", true))
    );
    let hints = json!({"readOnlyHint": true, "destructiveHint": true});
    let mcp = tool_meta("mcp__srv__tool", Some(&hints)).unwrap();
    assert_eq!(
        (mcp.scope, mcp.read_only, mcp.destructive),
        ("network", true, true)
    );
    assert_eq!(
        tool_meta("mcp__node_repl__js", None).unwrap().scope,
        "system"
    );
    assert_eq!(tool_meta("Unknown", None), None);
}

#[test]
fn model_usage_normalizes_like_node() {
    let raw = json!({"prompt_tokens": 130, "completion_tokens": 7,
        "prompt_tokens_details": {"cached_tokens": 20, "cache_write_tokens": 10},
        "server_tool_use": {"web_search_requests": 1}});
    assert_eq!(
        model_usage(&raw).to_string(),
        r#"{"inputTokens":130,"outputTokens":7,"totalTokens":137,"cacheReadTokens":20,"cacheWriteTokens":10,"serverToolUse":{"webSearchRequests":1}}"#
    );
    assert_eq!(model_usage(&json!({})), serde_json::Value::Null);
}
