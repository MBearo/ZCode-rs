#![expect(clippy::disallowed_methods, reason = "测试检查写出的日志文件")]

use std::sync::atomic::AtomicU64;

use rust_cli_kernel_api::testing::FixedClock;
use serde_json::Value;
use tokio::sync::oneshot;

use super::*;
use crate::log::Item;

fn read(dir: &Path, date: &str) -> String {
    std::fs::read_to_string(dir.join(format!("zcode-{date}.jsonl"))).unwrap_or_default()
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn a_new_day_writes_a_new_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut files = DailyFiles::new(dir.path().join("log"), 1024);
    files.append("2026-10-18", 0, b"a\n").await;
    files.append("2026-10-18", 0, b"b\n").await;
    files.append("2026-10-19", 0, b"c\n").await;
    let log = dir.path().join("log");
    assert_eq!(read(&log, "2026-10-18"), "a\nb\n");
    assert_eq!(read(&log, "2026-10-19"), "c\n");
}

#[tokio::test]
async fn the_daily_cap_writes_one_notice_and_drops_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let mut files = DailyFiles::new(dir.path().to_path_buf(), 10);
    files.append("2026-10-18", 0, b"123456\n").await;
    files.append("2026-10-18", 0, b"abcdef\n").await;
    files.append("2026-10-18", 0, b"zz\n").await;
    let text = read(dir.path(), "2026-10-18");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert_eq!(lines[0], "123456");
    let notice: Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(notice["event"], "log.capped");
    assert_eq!(notice["context"]["limitBytes"], 10);
    // 第二天重新计数。
    files.append("2026-10-19", 0, b"next\n").await;
    assert_eq!(read(dir.path(), "2026-10-19"), "next\n");
}

#[tokio::test]
async fn an_existing_file_counts_toward_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("zcode-2026-10-18.jsonl"), "0123456789\n").unwrap();
    let mut files = DailyFiles::new(dir.path().to_path_buf(), 10);
    files.append("2026-10-18", 0, b"x\n").await;
    files.append("2026-10-18", 0, b"y\n").await;
    // 打开时已经到上限：提示由先写满它的那个进程写过，这里不再追加任何行。
    assert_eq!(read(dir.path(), "2026-10-18"), "0123456789\n");
}

#[tokio::test]
async fn files_older_than_seven_days_are_removed_when_a_day_opens() {
    let dir = tempfile::tempdir().unwrap();
    for name in [
        "zcode-2026-10-01.jsonl",
        "zcode-2026-10-10.jsonl",
        "zcode-2026-10-11.jsonl",
        "zcode-2026-10-17.jsonl",
        "zcode-notes.jsonl",
        "other.txt",
    ] {
        std::fs::write(dir.path().join(name), "").unwrap();
    }
    let mut files = DailyFiles::new(dir.path().to_path_buf(), 1024);
    files.append("2026-10-18", 0, b"today\n").await;
    assert_eq!(
        names(dir.path()),
        [
            "other.txt",
            "zcode-2026-10-11.jsonl",
            "zcode-2026-10-17.jsonl",
            "zcode-2026-10-18.jsonl",
            "zcode-notes.jsonl"
        ]
    );
}

#[test]
fn the_cutoff_crosses_month_and_year_boundaries() {
    assert_eq!(days_before("2026-10-18", 7).as_deref(), Some("2026-10-11"));
    assert_eq!(days_before("2026-03-03", 7).as_deref(), Some("2026-02-24"));
    assert_eq!(days_before("2024-03-03", 7).as_deref(), Some("2024-02-25"));
    assert_eq!(days_before("2027-01-02", 7).as_deref(), Some("2026-12-26"));
    assert_eq!(days_before("not a date", 7), None);
}

#[tokio::test]
async fn flush_answers_after_the_earlier_lines_are_written() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().to_path_buf();
    let (sender, receiver) = mpsc::channel(16);
    let dropped = Arc::new(AtomicU64::new(3));
    // 2026-10-18T00:00:00Z
    let clock = Arc::new(FixedClock(1_792_281_600_000));
    let files = DailyFiles::new(log.clone(), 1024);
    let writer = write_loop(receiver, files, clock, dropped.clone());
    let driver = async {
        sender
            .send(Item::Line("{\"message\":\"one\"}".to_owned()))
            .await
            .unwrap();
        let (reply, flushed) = oneshot::channel();
        sender.send(Item::Flush(reply)).await.unwrap();
        flushed.await.unwrap();
        let text = read(&log, "2026-10-18");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        // 先补丢弃提示，再写这一批。
        let notice: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(notice["event"], "log.dropped");
        assert_eq!(notice["context"]["dropped"], 3);
        assert_eq!(lines[1], "{\"message\":\"one\"}");
        drop(sender);
    };
    tokio::join!(writer, driver);
    assert_eq!(dropped.load(std::sync::atomic::Ordering::Relaxed), 0);
}
