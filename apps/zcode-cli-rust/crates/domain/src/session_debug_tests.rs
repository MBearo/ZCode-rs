//! Node `observeSessionDebug` rules: 5 status types, bounded headers, rounds
//! only for new main-turn completions, unknown cache usage disables the rate.
use super::*;

fn completed(request: &str, usage: Value) -> Value {
    json!({"type": "model_request_completed", "timestamp": "2026-09-24T08:00:00.500Z",
        "requestId": request, "providerId": "p", "modelId": "m", "querySource": "main_turn",
        "attempt": 1, "maxAttempts": 11, "durationMs": 1000, "timeToFirstContentMs": 200,
        "usage": usage, "requestHeaders": {"x-request-id": request, "n": 1}, "responseHeaders": {},
        "finishReason": "stop"})
}

#[test]
fn entries_follow_the_node_mapper() {
    let mut log = DebugLog::default();
    log.observe(&json!({"type": "model_request_queued"}), "e0", "t", 7);
    log.observe(
        &json!({"type": "model_request_started", "attempt": 0, "maxAttempts": 0,
        "message": "", "statusCode": 429, "requestHeaders": {"a": "x".repeat(600)}}),
        "e1",
        "t",
        7,
    );
    log.observe(&json!({"type": "model_request_started"}), "e1", "t", 8);
    let snapshot = log.snapshot("s");
    let entries = snapshot["networkEntries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry["recordedAt"], 7);
    assert_eq!(entry["maxAttempts"], 0);
    assert_eq!(entry.get("attempt"), None);
    assert_eq!(entry.get("message"), None);
    assert_eq!(entry["statusCode"], 429);
    assert_eq!(entry["requestHeaders"]["a"].as_str().unwrap().len(), 512);
    assert_eq!(
        (
            entry["requestHeaderCount"].clone(),
            entry["responseHeaderCount"].clone()
        ),
        (json!(1), json!(0))
    );
    assert_eq!(snapshot["cache"], Value::Null);
}

#[test]
fn rounds_count_new_main_turn_completions() {
    let mut log = DebugLog::default();
    log.observe(
        &completed(
            "r1",
            json!({"inputTokens": 100, "outputTokens": 40, "cacheReadTokens": 60}),
        ),
        "e1",
        "t",
        0,
    );
    log.observe(&completed("r1", json!({"inputTokens": 1})), "e2", "t", 0);
    let mut compact = completed("r2", json!({"inputTokens": 5}));
    compact["querySource"] = "compact".into();
    log.observe(&compact, "e3", "t", 0);
    let snapshot = log.snapshot("s");
    assert_eq!(snapshot["networkEntries"].as_array().unwrap().len(), 3);
    assert_eq!(
        snapshot["rounds"],
        json!([{"eventKey": "e1", "requestId": "r1", "requestIndex": 1,
            "recordedAt": 1_790_236_800_500u64,
            "usage": {"inputTokens": 100, "outputTokens": 40, "totalTokens": 140, "cachedInputTokens": 60},
            "hitRate": 0.6, "generationDurationMs": 800, "tokensPerSecond": 50}])
    );
    assert_eq!(
        snapshot["cache"],
        json!({"hitRateRequestCount": 1, "totalInputTokens": 100, "totalCacheReadTokens": 60, "hitRate": 0.6})
    );
    // 缺少缓存读数：累计命中率此后恒为 null。
    log.observe(&completed("r3", json!({"inputTokens": 10})), "e4", "t", 0);
    assert_eq!(log.snapshot("s")["cache"]["hitRate"], Value::Null);
}
