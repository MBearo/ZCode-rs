// 复制自 rust/crates/kit/src/fs/stamp_tests.rs（feat/rs 79b6691c55）。
use std::time::{Duration, Instant};

use super::*;

const FULL_READ_EVERY: Duration = Duration::from_secs(60);

async fn stamps(paths: &[&Path]) -> Vec<FileStamp> {
    let mut stamps = Vec::new();
    for path in paths {
        stamps.push(FileStamp::of(path).await);
    }
    stamps
}

fn set_modified(path: &Path, modified: SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
}

#[tokio::test]
async fn stamp_changes_when_the_file_appears_grows_or_disappears() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("provider_config.json");
    let missing = FileStamp::of(&file).await;
    assert_eq!(missing, FileStamp::of(&file).await);
    std::fs::write(&file, "{}").unwrap();
    let present = FileStamp::of(&file).await;
    assert_ne!(present, missing);
    assert_eq!(present, FileStamp::of(&file).await);
    let modified = std::fs::metadata(&file).unwrap().modified().unwrap();
    std::fs::write(&file, "{\"a\":1}").unwrap();
    set_modified(&file, modified);
    assert_ne!(FileStamp::of(&file).await, present);
    std::fs::remove_file(&file).unwrap();
    assert_eq!(FileStamp::of(&file).await, missing);
}

/// 原子写（临时文件 + rename）换了 inode：长度与 mtime 都相同也看得出。
#[cfg(unix)]
#[tokio::test]
async fn rename_over_with_same_size_and_mtime_changes_the_stamp() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("zcode-builtin.json");
    std::fs::write(&file, "{\"revision\":1}").unwrap();
    let modified = std::fs::metadata(&file).unwrap().modified().unwrap();
    let before = FileStamp::of(&file).await;
    let temp = directory.path().join(".zcode-builtin.json.tmp");
    std::fs::write(&temp, "{\"revision\":2}").unwrap();
    set_modified(&temp, modified);
    std::fs::rename(&temp, &file).unwrap();
    assert_ne!(FileStamp::of(&file).await, before);
}

/// 原地改写、长度不变、mtime 被改回：Unix 上 ctime 变了；其他平台指纹相同，靠兜底放行。
#[tokio::test]
async fn in_place_rewrite_with_same_size_and_mtime() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("provider_config.json");
    std::fs::write(&file, "{\"a\":1}").unwrap();
    let modified = std::fs::metadata(&file).unwrap().modified().unwrap();
    let before = FileStamp::of(&file).await;
    // ctime 的粒度可能是一个时钟节拍：等过一个节拍再改写。
    tokio::time::sleep(Duration::from_millis(20)).await;
    std::fs::write(&file, "{\"a\":2}").unwrap();
    set_modified(&file, modified);
    assert_eq!(FileStamp::of(&file).await != before, cfg!(unix));
}

#[tokio::test]
async fn gate_reads_until_recorded_then_only_on_change_or_after_the_full_read_interval() {
    let directory = tempfile::tempdir().unwrap();
    let (builtin, personal) = (
        directory.path().join("builtin.json"),
        directory.path().join("personal.json"),
    );
    std::fs::write(&builtin, "{}").unwrap();
    let start = Instant::now();
    let mut gate = ChangeGate::new(FULL_READ_EVERY);
    let first = stamps(&[&builtin, &personal]).await;
    assert!(gate.should_read(&first, start));
    // 读失败（没有 record）时下一轮照常读。
    assert!(gate.should_read(&first, start + Duration::from_secs(1)));
    gate.record(first.clone(), start);
    let tick = start + Duration::from_secs(1);
    assert!(!gate.should_read(&stamps(&[&builtin, &personal]).await, tick));
    // 个人配置出现：指纹变了。
    std::fs::write(&personal, "{}").unwrap();
    let appeared = stamps(&[&builtin, &personal]).await;
    assert!(gate.should_read(&appeared, tick));
    gate.record(appeared.clone(), tick);
    // 路径换了（例如 Endpoint 切换后的 Active 路径）：即使文件相同也要读。
    let other = directory.path().join("other.json");
    std::fs::copy(&builtin, &other).unwrap();
    assert!(gate.should_read(&stamps(&[&other, &personal]).await, tick));
    // 指纹一直不变：满兜底间隔才放行。
    let almost = tick + FULL_READ_EVERY - Duration::from_millis(1);
    assert!(!gate.should_read(&appeared, almost));
    assert!(gate.should_read(&appeared, tick + FULL_READ_EVERY));
}
