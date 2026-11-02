use std::sync::atomic::Ordering;

use rust_cli_base::js::iso_timestamp;
use rust_cli_kernel_api::testing::FixedClock;
use serde_json::Value;

use super::*;

const NOW: u64 = 1_760_756_645_678;

/// 在一个临时的线程级订阅者下发事件，取出写进通道的行与丢弃数。
fn capture(max: LevelFilter, capacity: usize, emit: impl FnOnce()) -> (Vec<Value>, u64) {
    let (sender, mut receiver) = mpsc::channel(capacity);
    let dropped = Arc::new(AtomicU64::new(0));
    let subscriber = FileSubscriber::new(max, Arc::new(FixedClock(NOW)), sender, dropped.clone());
    tracing::subscriber::with_default(subscriber, emit);
    let mut lines = Vec::new();
    while let Ok(Item::Line(line)) = receiver.try_recv() {
        lines.push(serde_json::from_str(&line).unwrap());
    }
    (lines, dropped.load(Ordering::Relaxed))
}

#[test]
fn a_line_has_the_node_shape_and_key_order() {
    let (lines, _) = capture(LevelFilter::INFO, 8, || {
        tracing::info!(
            event = "model.request.end",
            session = "sess_1",
            tool_use = "toolu_1",
            duration_ms = 12_u64,
            status = "completed",
            model = "m",
            stop = ?Some("end_turn"),
            retried = false,
            elapsed = 7_u128,
            error = %"boom",
            "model request finished"
        );
    });
    let line = &lines[0];
    let keys: Vec<&str> = line
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "timestamp",
            "level",
            "event",
            "module",
            "message",
            "sessionId",
            "toolCallId",
            "durationMs",
            "status",
            "context",
            "error"
        ]
    );
    assert_eq!(line["timestamp"], iso_timestamp(NOW));
    assert_eq!(line["level"], "info");
    assert_eq!(line["event"], "model.request.end");
    assert_eq!(line["module"], module_path!());
    assert_eq!(line["message"], "model request finished");
    assert_eq!(line["sessionId"], "sess_1");
    assert_eq!(line["toolCallId"], "toolu_1");
    assert_eq!(line["durationMs"], 12);
    assert_eq!(line["status"], "completed");
    assert_eq!(
        line["context"],
        serde_json::json!({"model": "m", "stop": "Some(\"end_turn\")", "retried": false, "elapsed": 7})
    );
    assert_eq!(line["error"], serde_json::json!({"message": "boom"}));
}

#[test]
fn a_numeric_status_and_a_missing_option_stay_out_of_the_reserved_keys() {
    let none: Option<&str> = None;
    let (lines, _) = capture(LevelFilter::INFO, 8, || {
        tracing::info!(status = 200_u64, agent = none, "http");
    });
    assert!(lines[0].get("status").is_none());
    assert_eq!(lines[0]["context"], serde_json::json!({"status": 200}));
}

#[test]
fn keys_without_values_are_left_out() {
    let (lines, _) = capture(LevelFilter::INFO, 8, || tracing::warn!("plain"));
    assert_eq!(
        lines[0].as_object().unwrap().keys().collect::<Vec<_>>(),
        ["timestamp", "level", "module", "message"]
    );
    assert_eq!(lines[0]["level"], "warn");
}

#[test]
fn secrets_are_redacted_by_field_name_and_by_value() {
    let (lines, _) = capture(LevelFilter::INFO, 8, || {
        tracing::info!(
            api_key = "abc123",
            authorization = "anything",
            input_tokens = 5_u64,
            note = "header Bearer abcdefghijklmnopqrstuvwxyz0123",
            "Bearer abcdefghijklmnopqrstuvwxyz0123 in the message"
        );
    });
    let context = &lines[0]["context"];
    assert_eq!(context["api_key"], "[Redacted]");
    assert_eq!(context["authorization"], "[Redacted]");
    // Node 的规则按字段名：带 token 的计数字段也会被遮掉（logging.md §5）。
    assert_eq!(context["input_tokens"], "[Redacted]");
    assert_eq!(context["note"], "header Bearer [REDACTED]");
    assert_eq!(lines[0]["message"], "Bearer [REDACTED] in the message");
}

#[test]
fn the_level_filter_drops_finer_events() {
    let (lines, _) = capture(LevelFilter::INFO, 8, || {
        tracing::trace!("never");
        tracing::debug!("only in development builds");
        tracing::info!("kept");
        tracing::error!("kept too");
    });
    let messages: Vec<&Value> = lines.iter().map(|line| &line["message"]).collect();
    assert_eq!(messages, ["kept", "kept too"]);
    let (lines, _) = capture(LevelFilter::DEBUG, 8, || {
        tracing::trace!("never");
        tracing::debug!("detail");
    });
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["level"], "debug");
}

#[test]
fn a_full_channel_drops_lines_and_counts_them() {
    let (lines, dropped) = capture(LevelFilter::INFO, 1, || {
        for index in 0..3 {
            tracing::info!(index, "line");
        }
    });
    assert_eq!(lines.len(), 1);
    assert_eq!(dropped, 2);
}

#[test]
fn development_builds_write_debug_lines() {
    let expected = if cfg!(debug_assertions) {
        LevelFilter::DEBUG
    } else {
        LevelFilter::INFO
    };
    assert_eq!(default_level(), expected);
}

#[test]
fn writer_notices_use_the_same_shape() {
    let mut context = Map::new();
    context.insert("dropped".to_owned(), Value::from(3));
    let line: Value = serde_json::from_str(&notice(
        NOW,
        "log.dropped",
        "log lines were dropped",
        context,
    ))
    .unwrap();
    assert_eq!(line["level"], "warn");
    assert_eq!(line["event"], "log.dropped");
    assert_eq!(line["module"], module_path!().trim_end_matches("::tests"));
    assert_eq!(line["context"]["dropped"], 3);
}
