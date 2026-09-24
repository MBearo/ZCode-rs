use super::*;
use serde_json::json;

fn text(row: u64, append: &str) -> Value {
    json!({"op":"row.delta","rowId":row,"path":"text","append":append})
}
fn upsert(row: u64, body: &str) -> Value {
    json!({"op":"row.upserted","row":{"rowId":row,"text":body}})
}
fn sized(values: Vec<Value>) -> Vec<(Value, usize)> {
    values
        .into_iter()
        .map(|v| {
            let size = json_bytes(&v);
            (v, size)
        })
        .collect()
}
fn coalesce(values: Vec<Value>) -> Vec<Value> {
    let mut buffer = Buffer::default();
    assert!(buffer.append(sized(values)));
    let bytes = buffer.payload_bytes();
    let out = buffer.take();
    // 字节记账必须等于实际 payload 的紧凑序列化长度。
    assert_eq!(
        bytes,
        json_bytes(&json!({"kind":"deltas","deltas":out})),
        "tracked bytes"
    );
    out
}

#[test]
fn profiles_filter_row_deltas_by_stream_path() {
    let input = json!({"op":"row.delta","rowId":1,"path":"inputText","append":"x"});
    let unknown = json!({"op":"row.delta","rowId":1,"path":"other","append":"x"});
    assert!(Profile::Continuous.keeps(&input));
    assert!(!Profile::Replayable.keeps(&input));
    assert!(Profile::Replayable.keeps(&text(1, "x")));
    assert!(
        !Profile::Continuous.keeps(&unknown),
        "Node streamPaths[path] is undefined"
    );
    assert!(Profile::Replayable.keeps(&upsert(1, "x")));
    assert_eq!(
        Profile::from_client_mode("desktop-continuous"),
        Profile::Continuous
    );
    assert_eq!(
        Profile::from_client_mode("web-remote-replayable"),
        Profile::Replayable
    );
    assert_eq!(Profile::Replayable.window(), Duration::from_millis(150));
}

#[test]
fn adjacent_text_and_state_updates_merge() {
    let out = coalesce(vec![
        text(1, "he"),
        text(1, "llo \"q\""),
        text(2, "x"),
        json!({"op":"state.updated","patch":{"a":1,"b":1}}),
        json!({"op":"state.updated","patch":{"b":2}}),
    ]);
    assert_eq!(
        out,
        vec![
            text(1, "hello \"q\""),
            text(2, "x"),
            json!({"op":"state.updated","patch":{"a":1,"b":2}})
        ]
    );
}

#[test]
fn upserts_swallow_earlier_row_deltas_until_a_barrier_or_the_previous_row() {
    let out = coalesce(vec![
        text(1, "a"),
        json!({"op":"state.updated","patch":{}}),
        text(1, "b"),
        upsert(1, "ab"),
    ]);
    assert_eq!(
        out,
        vec![json!({"op":"state.updated","patch":{}}), upsert(1, "ab")]
    );
    let barrier = coalesce(vec![
        text(1, "a"),
        json!({"op":"row.removed","fromRowId":5}),
        upsert(1, "ab"),
    ]);
    assert_eq!(barrier.len(), 3, "row.removed is a barrier");
    let previous = coalesce(vec![
        text(1, "old"),
        json!({"op":"row.appended","row":{"rowId":1}}),
        text(1, "new"),
        upsert(1, "new"),
    ]);
    assert_eq!(
        previous,
        vec![
            text(1, "old"),
            json!({"op":"row.appended","row":{"rowId":1}}),
            upsert(1, "new")
        ]
    );
    assert_eq!(
        coalesce(vec![upsert(3, "1"), upsert(3, "2")]),
        vec![upsert(3, "2")]
    );
}

#[test]
fn the_buffer_overflows_on_ops_or_bytes() {
    let mut ops = Buffer::default();
    let many = (0..=MAX_OPS as u64).map(|i| text(i, "x")).collect();
    assert!(!ops.append(sized(many)), "501 distinct rows");
    let mut merged = Buffer::default();
    let same = (0..1000).map(|_| text(1, "x")).collect();
    assert!(merged.append(sized(same)), "one merged row");
    let mut bytes = Buffer::default();
    assert!(!bytes.append(sized(vec![text(1, &"y".repeat(MAX_BYTES))])));
}

#[test]
fn replay_filters_then_coalesces() {
    let deltas = vec![
        text(1, "a"),
        json!({"op":"row.delta","rowId":2,"path":"output.text","append":"o"}),
        text(1, "b"),
    ];
    assert_eq!(
        replay(Profile::Replayable, deltas.clone()),
        vec![text(1, "ab")]
    );
    assert_eq!(replay(Profile::Continuous, deltas).len(), 3);
}
