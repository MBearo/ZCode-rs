//! Node `SO:3046-3104` batching: first delta immediately, then by size or by
//! event-time interval; any other event flushes and resets.
use super::*;

fn delta(id: &str, text: &str, at: u64) -> Value {
    json!({"eventId": id, "sessionId": "s", "timestamp": at, "type": "model.streaming",
        "payload": {"assistantMessageId": "m", "delta": text, "done": false, "kind": "text_delta"}})
}

fn other(id: &str, at: u64) -> Value {
    json!({"eventId": id, "sessionId": "s", "timestamp": at, "type": "turn.completed", "payload": {}})
}

fn stream() -> LegacyStream {
    let mut stream = LegacyStream::default();
    assert_eq!(stream.subscribe("web-remote-replayable"), Some(0));
    stream
}

fn texts(out: &[Value]) -> Vec<(u64, String, String)> {
    out.iter()
        .map(|e| {
            (
                e["seq"].as_u64().unwrap(),
                e["eventId"].as_str().unwrap().to_owned(),
                e["payload"]["delta"].as_str().unwrap_or("").to_owned(),
            )
        })
        .collect()
}

#[test]
fn first_delta_goes_out_then_deltas_merge_until_a_barrier() {
    let mut s = stream();
    let mut out = vec![];
    s.push(delta("a", "He", 0), &mut out);
    s.push(delta("b", "ll", 10), &mut out);
    s.push(delta("c", "o", 20), &mut out);
    assert_eq!(texts(&out), [(1, "a".into(), "He".into())]);
    s.push(other("d", 30), &mut out);
    // 合并事件沿用最后一个增量的 eventId 与时间。
    assert_eq!(
        texts(&out)[1..],
        [
            (2, "c".into(), "llo".into()),
            (3, "d".into(), String::new())
        ]
    );
    assert_eq!(out[1]["timestamp"], 20);
    assert_eq!(out[1]["deliveryKind"], "web-remote-replayable");
    // 屏障清空"已首发"：下一段文本的首个增量再次立即发出。
    s.push(delta("e", "x", 40), &mut out);
    assert_eq!(texts(&out)[3], (4, "e".into(), "x".into()));
    assert_eq!(s.seq(), 4);
}

#[test]
fn merged_deltas_flush_by_interval_and_size() {
    let mut s = stream();
    let mut out = vec![];
    s.push(delta("a", "1", 0), &mut out);
    s.push(delta("b", "2", 100), &mut out);
    s.push(delta("c", "3", 249), &mut out);
    assert_eq!(out.len(), 1);
    // 距上次发出（首个增量，时间 0）满 250 ms。
    s.push(delta("d", "4", 250), &mut out);
    assert_eq!(texts(&out)[1], (2, "d".into(), "234".into()));
    // 2048 个 UTF-16 码元（每个汉字 1 个，emoji 2 个）。
    s.push(delta("e", &"字".repeat(2046), 260), &mut out);
    assert_eq!(out.len(), 2);
    s.push(delta("f", "😀", 270), &mut out);
    assert_eq!(out.len(), 3);
    assert_eq!(out[2]["eventId"], "f");
}

#[test]
fn a_new_key_flushes_the_pending_batch_but_keeps_first_flags() {
    let mut s = stream();
    let mut out = vec![];
    s.push(delta("a", "x", 0), &mut out);
    s.push(delta("b", "y", 1), &mut out);
    let mut reasoning = delta("c", "r", 2);
    reasoning["payload"]["kind"] = "reasoning_delta".into();
    s.push(reasoning, &mut out);
    assert_eq!(
        texts(&out),
        [
            (1, "a".into(), "x".into()),
            (2, "b".into(), "y".into()),
            (3, "c".into(), "r".into()),
        ]
    );
    // 回到已首发的文本键：不再立即发出。
    s.push(delta("d", "z", 3), &mut out);
    assert_eq!(out.len(), 3);
    s.barrier(&mut out);
    assert_eq!(texts(&out)[3], (4, "d".into(), "z".into()));
}

#[test]
fn domains_count_separately_and_nothing_goes_out_unsubscribed() {
    let mut s = LegacyStream::default();
    let mut out = vec![];
    s.push(other("a", 0), &mut out);
    assert!(out.is_empty());
    assert_eq!(s.subscribe("desktop-continuous"), Some(0));
    s.push(other("b", 1), &mut out);
    assert_eq!(s.subscribe("web-remote-replayable"), Some(0));
    s.push(other("c", 2), &mut out);
    assert_eq!(s.subscribe("desktop-continuous"), Some(1));
    assert_eq!(s.subscribe("other"), None);
    assert_eq!(out[1]["seq"], 1);
    assert_eq!(out[1]["deliveryKind"], "web-remote-replayable");
}

#[test]
fn tallies_turn_usage_like_node() {
    let usage = model_usage(&json!({"prompt_tokens": 10, "completion_tokens": 4,
        "prompt_tokens_details": {"cached_tokens": 6}}));
    assert_eq!(usage, [10, 4, 14, 6, 0, 0]);
    let mut turn = TurnTally::default();
    turn.model_done(usage, "Hel", 1);
    turn.model_done(usage, "lo", 0);
    assert_eq!(turn.response, "Hello");
    assert_eq!((turn.token_count, turn.tool_calls, turn.rounds), (28, 1, 2));
    assert_eq!(
        turn.summary(),
        json!({"source": "provider", "modelRequestCount": 2, "inputTokens": 20,
            "outputTokens": 8, "totalTokens": 28, "cacheReadTokens": 12, "cacheWriteTokens": 0,
            "reasoningTokens": 0, "webSearchRequests": 0, "webFetchRequests": 0})
    );
}
