use super::*;

fn event<'a>(kind: &'a str, turn: Option<&'a str>, payload: &'a Value) -> Event<'a> {
    Event {
        id: "e",
        seq: 3,
        at: 1000,
        session: "s",
        turn,
        kind,
        payload,
    }
}

fn fact(n: &mut Normalizer, kind: &str, turn: Option<&str>, payload: Value) -> Option<Value> {
    n.normalize(&event(kind, turn, &payload), &Runtime::default())
}

#[test]
fn a_turn_carries_its_admission_command() {
    let mut n = Normalizer::default();
    let started = fact(&mut n, "turn_started", Some("t"), json!({"inputId": "c1"})).unwrap();
    assert_eq!(
        started,
        json!({"version": 1, "eventId": "e", "eventSeq": 3, "occurredAt": 1000, "sessionId": "s",
            "sourceCommandId": "c1", "turnId": "t", "kind": "turn.started"})
    );
    let status = json!({"type": "model_request_started", "requestId": "r1", "providerId": "p",
        "modelId": "m", "baseURL": "https://API.Example.com:8443/v1", "providerKind": "anthropic",
        "transport": "sse", "querySource": "main_turn", "queryId": "c1", "attempt": 1,
        "maxAttempts": 11, "requestHeaders": {}});
    let started = fact(&mut n, "model_network_status", Some("t"), status).unwrap();
    assert_eq!(started["sourceCommandId"], "c1");
    assert_eq!(started["providerHostname"], "api.example.com");
    assert!(started.get("requestHeaders").is_none());
    let done = json!({"resultType": "success", "duration": 5, "tokenCount": 9, "toolCallCount": 0});
    let terminal = fact(&mut n, "turn_complete", Some("t"), done).unwrap();
    assert_eq!(terminal["status"], "success");
    // 轮次结束后映射清除：同轮次之后的请求不再带命令 id。
    let late = json!({"type": "model_request_started", "requestId": "r2", "providerId": "p",
        "modelId": "m", "transport": "http", "attempt": 1, "maxAttempts": 1});
    let late = fact(&mut n, "model_network_status", Some("t"), late).unwrap();
    assert!(late.get("sourceCommandId").is_none());
    // 未知的 inputSource 让 Node 的 schema 丢弃整条事实。
    let unknown = json!({"inputId": "c2", "inputSource": "other"});
    assert_eq!(fact(&mut n, "turn_started", Some("u"), unknown), None);
    let queued = json!({"type": "model_request_queued", "requestId": "r"});
    assert_eq!(fact(&mut n, "model_network_status", None, queued), None);
}

#[test]
fn chunks_mark_the_first_of_each_stream() {
    let mut n = Normalizer::default();
    let delta =
        |text: &str| json!({"kind": "text_delta", "delta": text, "assistantMessageId": "a"});
    let first = fact(&mut n, "model_streaming", Some("t"), delta("hé😀")).unwrap();
    assert_eq!(first["chunkLength"], 4);
    assert_eq!(first["firstChunk"], true);
    let second = fact(&mut n, "model_streaming", Some("t"), delta("")).unwrap();
    assert_eq!(second["firstChunk"], false);
    assert_eq!(second["chunkLength"], 0);
    let thought = json!({"kind": "reasoning_delta", "delta": "x"});
    assert_eq!(
        fact(&mut n, "model_streaming", Some("t"), thought).unwrap()["channel"],
        "thought"
    );
    let other = json!({"kind": "tool_input_delta", "delta": "x"});
    assert_eq!(fact(&mut n, "model_streaming", Some("t"), other), None);
}

#[test]
fn usage_takes_the_completed_request_identity() {
    let mut n = Normalizer::default();
    let completed = json!({"type": "model_request_completed", "requestId": "r1", "providerId": "p",
        "modelId": "m", "providerKind": "anthropic", "transport": "sse", "querySource": "main_turn",
        "attempt": 1, "maxAttempts": 1, "durationMs": 7});
    fact(&mut n, "model_network_status", Some("t"), completed).unwrap();
    let usage = json!({"querySource": "main_turn", "stopReason": "stop",
        "usage": {"inputTokens": 130, "outputTokens": 7, "cacheReadTokens": 20, "cacheWriteTokens": 10}});
    let delta = fact(&mut n, "model_complete", Some("t"), usage.clone()).unwrap();
    assert_eq!(delta["requestId"], "r1");
    assert_eq!(delta["providerKind"], "anthropic");
    assert_eq!(delta["totalTokens"], 137);
    assert_eq!(delta["reasoningTokens"], 0);
    // 队列已空：第二次完成没有请求身份。
    let again = fact(&mut n, "model_complete", Some("t"), usage).unwrap();
    assert!(again.get("requestId").is_none());
    let compact = json!({"querySource": "compact", "stopReason": "stop", "usage": {}});
    assert_eq!(fact(&mut n, "model_complete", Some("t"), compact), None);
    let nested = json!({"stopReason": "tool_internal", "usage": {"inputTokens": 1}});
    assert_eq!(fact(&mut n, "model_complete", Some("t"), nested), None);
}

#[test]
fn tools_remember_their_names_and_map_performance() {
    let mut n = Normalizer::default();
    fact(&mut n, "turn_started", Some("t"), json!({"inputId": "c"}));
    let scheduled = json!({"toolCallId": "k", "toolName": "Bash"});
    fact(&mut n, "tool_call_scheduled", Some("t"), scheduled).unwrap();
    let started = fact(
        &mut n,
        "tool_call_started",
        Some("t"),
        json!({"toolCallId": "k"}),
    )
    .unwrap();
    assert_eq!(started["toolName"], "Bash");
    assert_eq!(started["sourceCommandId"], "c");
    let perf = json!({"totalMs": 28, "detail": {"kind": "command", "command": {"runMs": 20,
        "exitCode": 0, "hash": "x", "status": "completed"}}});
    let result = json!({"toolCallId": "k", "duration": 30,
        "result": {"success": true, "content": "ok", "perf": perf}});
    let done = fact(&mut n, "tool_call_result", Some("t"), result).unwrap();
    assert_eq!(done["phase"], "completed");
    assert_eq!(
        done["performance"],
        json!({"totalMs": 28, "commandRunMs": 20, "exitCode": 0, "commandStatus": "completed"})
    );
    let error = json!({"toolCallId": "k", "error": {"type": "tool_execution_failed",
        "code": "TOOL_EXECUTION_FAILED", "message": "boom"}});
    let failed = fact(&mut n, "tool_call_error", Some("t"), error).unwrap();
    // 名字在终态时被移除。
    assert!(failed.get("toolName").is_none());
    assert_eq!(failed["errorCode"], "TOOL_EXECUTION_FAILED");
}

#[test]
fn permissions_subagents_and_compaction() {
    let mut n = Normalizer::default();
    let requested = json!({"requestId": "q", "toolCallId": "k", "toolName": "Bash"});
    let fact1 = fact(&mut n, "permission_requested", Some("t"), requested).unwrap();
    assert_eq!(fact1["toolName"], "Bash");
    let resolved =
        json!({"requestId": "q", "toolCallId": "k", "decision": "deny", "toolName": "x"});
    let fact2 = fact(&mut n, "permission_resolved", Some("t"), resolved).unwrap();
    assert!(fact2.get("toolName").is_none());
    assert_eq!(fact2["decision"], "deny");
    let spawned = json!({"agentId": "a", "childSessionId": "c", "agentType": "general-purpose",
        "parentToolCallId": "k", "status": "running"});
    let fact3 = fact(&mut n, "subagent_spawned", Some("t"), spawned).unwrap();
    assert_eq!(fact3["background"], false);
    assert_eq!(
        fact(
            &mut n,
            "subagent_stopped",
            Some("t"),
            json!({"agentId": "a"})
        ),
        None
    );
    let compact = json!({"operationId": "o", "status": "completed", "trigger": "manual",
        "sourceCommandId": "c", "preCompactTokenCount": 10});
    let runtime = Runtime {
        memory_enabled: Some(false),
        model: Some(("m".into(), "p".into())),
    };
    let payload = compact;
    let terminal = n
        .normalize(&event("compact_completed", Some("t"), &payload), &runtime)
        .unwrap();
    assert_eq!(terminal["modelName"], "m");
    assert_eq!(terminal["memoryEnabled"], false);
    assert_eq!(terminal["sourceCommandId"], "c");
    let skipped = json!({"operationId": "o", "status": "skipped", "trigger": "manual"});
    assert_eq!(fact(&mut n, "compact_completed", Some("t"), skipped), None);
}

#[test]
fn computer_use_marks_node_repl_runtime_calls() {
    let input = json!({"toolCallId": "k", "toolName": "mcp__node_repl__js",
        "input": {"code": "await setupComputerUseRuntime()"}});
    let scheduled = computer_use(&event("tool_call_scheduled", Some("t"), &input)).unwrap();
    assert_eq!(
        scheduled,
        json!({"eventId": "e", "sequenceNumber": 3, "sessionId": "s", "timestamp": 1000,
            "kind": "tool-scheduled", "turnId": "t", "toolCallId": "k",
            "toolName": "mcp__node_repl__js", "computerUse": true})
    );
    let started = json!({"toolCallId": "k"});
    let started = computer_use(&event("tool_call_started", None, &started)).unwrap();
    assert!(started.get("turnId").is_none());
    assert_eq!(
        computer_use(&event("turn_started", None, &Value::Null)),
        None
    );
    assert_eq!(
        computer_use(&event("model_complete", Some("t"), &Value::Null)),
        None
    );
}
