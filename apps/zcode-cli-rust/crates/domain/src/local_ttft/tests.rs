use super::*;
use serde_json::json;

fn recorder() -> Recorder {
    let mut r = Recorder::new("inst".into());
    let ttft = json!({"version": 1, "observationId": "obs"});
    assert!(r.receive("c1", Some("s"), (&ttft, false), (100.0, 100.0)));
    r
}

fn status(status: &str, request: &str, source: &str) -> Value {
    json!({"kind": "model.request.status", "sessionId": "s", "turnId": "t", "sourceCommandId": "c1",
        "status": status, "requestId": request, "querySource": source, "queryId": "q",
        "providerId": "p", "modelId": "m"})
}

#[test]
fn a_prompt_records_its_way_to_the_first_text() {
    let mut r = recorder();
    r.admitted("c1", 101.0);
    let started =
        json!({"kind": "turn.started", "sessionId": "s", "turnId": "t", "sourceCommandId": "c1"});
    r.fact(&started, Some(102.0), 102.5);
    r.fact(
        &status("model_request_started", "r1", "main_turn"),
        None,
        110.0,
    );
    r.fact(
        &status("model_retry_scheduled", "r1", "main_turn"),
        None,
        111.0,
    );
    r.fact(
        &status("model_request_started", "r2", "main_turn"),
        None,
        115.0,
    );
    r.logical_call("s", "r2", "call-1", 116.0);
    r.output("s", Some("t"), "reasoning", 120.0);
    r.output("s", Some("t"), "text", 121.0);
    let sent = r.take_checkpoints();
    let last = sent.last().unwrap();
    assert_eq!(
        sent.iter().map(|f| f.revision.unwrap()).collect::<Vec<_>>(),
        [1, 2, 3, 4, 5, 6]
    );
    assert_eq!(
        (last.admitted_at, last.execution_at, last.request_at),
        (Some(101.0), Some(102.0), Some(110.0))
    );
    assert_eq!(last.request_id.as_deref(), Some("r2"));
    // 首个输出（推理）之后没有再发 checkpoint；首个正文让记录退役。
    assert!(!r.active());
    let kept = r.for_session("s", Some("c1"), 130.0).unwrap();
    assert_eq!(
        (kept.output_at, kept.output_kind),
        (Some(120.0), Some("reasoning"))
    );
    let retry = kept
        .details
        .iter()
        .find(|d| d.stage == "retry_wait")
        .unwrap();
    assert_eq!((retry.end, retry.outcome), (Some(115.0), Some("completed")));
    let attempt = kept.details.iter().find(|d| d.id == "attempt:r2").unwrap();
    assert_eq!(attempt.outcome, Some("first_output"));
    // 一次调用的 logicalCallId 记在当前请求与它的 attempt 上。
    assert_eq!(attempt.logical_call_id.as_deref(), Some("call-1"));
    assert_eq!(kept.logical_call_id.as_deref(), Some("call-1"));
    let json = kept.attached("1.0.0", Some("t"));
    assert_eq!(json["cliVersion"], "1.0.0");
    assert_eq!(json["productTurnId"], "t");
    assert!(json.get("clockInvalid").is_none());
}

#[test]
fn compaction_and_failures_hold_the_output() {
    let mut r = recorder();
    let started =
        json!({"kind": "turn.started", "sessionId": "s", "turnId": "t", "sourceCommandId": "c1"});
    r.fact(&started, None, 101.0);
    r.fact(
        &status("model_request_started", "k", "compact"),
        None,
        102.0,
    );
    r.compaction(
        "s",
        Some("t"),
        ("compact_started", &json!({"operationId": "o"})),
        102.0,
    );
    r.compaction(
        "s",
        Some("t"),
        (
            "compact_completed",
            &json!({"operationId": "o", "status": "completed"}),
        ),
        104.0,
    );
    // 压缩请求是 preparation：此时的输出不算首个输出。
    r.output("s", Some("t"), "text", 105.0);
    assert!(r.active());
    r.fact(
        &status("model_request_started", "r1", "main_turn"),
        None,
        106.0,
    );
    r.fact(
        &status("model_request_failed", "r1", "main_turn"),
        None,
        107.0,
    );
    r.output("s", Some("t"), "text", 108.0);
    let terminal = json!({"kind": "turn.terminal", "sessionId": "s", "turnId": "t",
        "sourceCommandId": "c1", "status": "failed"});
    r.fact(&terminal, None, 109.0);
    let last = r.take_checkpoints().pop().unwrap();
    assert_eq!(last.terminal, Some("failed"));
    assert_eq!(last.output_at, None);
    let compact = last
        .details
        .iter()
        .find(|d| d.stage == "compaction")
        .unwrap();
    assert_eq!(
        (compact.end, compact.outcome),
        (Some(104.0), Some("completed"))
    );
}

#[test]
fn capacity_clock_and_queue() {
    let mut r = Recorder::new("inst".into());
    let ttft = json!({"version": 1, "observationId": "obs"});
    for i in 0..128 {
        assert!(r.receive(&format!("c{i}"), None, (&ttft, true), (0.0, 0.0)));
    }
    assert!(!r.receive("over", None, (&ttft, false), (0.0, 0.0)));
    r.tick(1000.0, 1000.0);
    r.tick(2000.0, 2500.0);
    let invalid = r.take_checkpoints();
    assert_eq!(invalid.len(), 128);
    assert!(
        invalid
            .iter()
            .all(|f| f.clock_invalid == Some(true) && f.send_mode == "queued")
    );
    r.guided("c0");
    r.discarded("c1", true);
    let sent = r.take_checkpoints();
    assert_eq!(
        (sent[0].send_mode, sent[1].terminal),
        ("guided", Some("failed"))
    );
    // 过期记录静默丢弃。
    r.tick(400_000.0, 400_000.0);
    assert!(!r.active());
}
