#![expect(clippy::disallowed_methods, reason = "测试检查写出的模型轨迹文件")]

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use futures_util::FutureExt;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::*;
use crate::model_io_files::Files;

#[test]
fn the_mode_follows_the_runtime_env_and_then_the_build() {
    assert_eq!(mode(Some("test"), true), Mode::Off);
    assert_eq!(mode(Some(" TEST "), false), Mode::Off);
    assert_eq!(mode(Some("development"), false), Mode::Development);
    assert_eq!(mode(Some("Production"), true), Mode::Production);
    assert_eq!(mode(Some("staging"), true), Mode::Development);
    assert_eq!(mode(None, true), Mode::Development);
    assert_eq!(mode(None, false), Mode::Production);
    assert_eq!(mode(Some(""), false), Mode::Production);
}

fn record(session: &str) -> Pending {
    let Value::Object(record) = json!({"type": "model_io", "sessionId": session}) else {
        unreachable!()
    };
    Pending {
        session: Some(session.to_owned()),
        started_ms: 1,
        build: Box::new(move || record),
    }
}

fn never() -> Retention {
    Arc::new(|| async { false }.boxed())
}

#[tokio::test]
async fn the_writer_notes_dropped_records_on_the_next_one() {
    let dir = tempfile::tempdir().unwrap();
    let (sender, receiver) = mpsc::channel(4);
    let dropped = Arc::new(AtomicU64::new(2));
    sender.send(Item::Record(record("s"))).await.unwrap();
    sender.send(Item::Record(record("s"))).await.unwrap();
    drop(sender);
    let files = Files::new(dir.path().to_path_buf(), true);
    write_loop(receiver, files, never(), dropped.clone()).await;
    let text = std::fs::read_to_string(dir.path().join("model-io-s.jsonl")).unwrap();
    let records: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["droppedRecords"], 2);
    assert!(records[1].get("droppedRecords").is_none());
    assert_eq!(dropped.load(std::sync::atomic::Ordering::Relaxed), 0);
}

#[tokio::test]
async fn flush_waits_for_the_queued_records() {
    let dir = tempfile::tempdir().unwrap();
    let model_io = ModelIo::default();
    // 没有打开过：立即返回。
    model_io.flush().await;
    let _sink = model_io.sink(Files::new(dir.path().to_path_buf(), false), never());
    // 与入队口同一个队列（记录换成测试构造的）。
    let sender = model_io.writer.get().unwrap().sender.clone();
    for _ in 0..3 {
        assert!(sender.try_send(Item::Record(record("s"))).is_ok());
    }
    model_io.flush().await;
    let text = std::fs::read_to_string(dir.path().join("model-io-s.jsonl")).unwrap();
    assert_eq!(text.lines().count(), 3);
}
