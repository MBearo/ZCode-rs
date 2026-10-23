//! 单文件锁（config.md §4.2）。
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

fn quick() -> SingleLockOptions {
    SingleLockOptions {
        retry: Duration::from_millis(5),
        timeout: Duration::from_millis(300),
        stale: Duration::from_millis(30_000),
    }
}

#[tokio::test]
async fn the_lock_file_is_exclusive_and_released_after_the_operation() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("store.json");
    let lock = lock_path(&file);
    let seen = with_single_file_lock(&file, &quick(), async {
        let text = tokio::fs::read_to_string(&lock).await?;
        Ok(text)
    })
    .await
    .unwrap();
    assert!(seen.starts_with(&format!("{{\"pid\":{},", std::process::id())));
    assert!(seen.ends_with("\"}\n"));
    assert!(!lock.exists());
    // 并发的两个持有者不会同时在临界区里。
    let inside = Arc::new(AtomicUsize::new(0));
    let run = |inside: Arc<AtomicUsize>, file: PathBuf| async move {
        with_single_file_lock(&file, &SingleLockOptions::default(), async {
            assert_eq!(inside.fetch_add(1, Ordering::SeqCst), 0);
            tokio::time::sleep(Duration::from_millis(20)).await;
            inside.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        })
        .await
    };
    let (a, b) = tokio::join!(
        run(inside.clone(), file.clone()),
        run(inside.clone(), file.clone())
    );
    a.unwrap();
    b.unwrap();
}

#[tokio::test]
async fn a_live_lock_times_out_and_a_stale_ownerless_lock_is_reclaimed() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("store.json");
    let lock = lock_path(&file);
    tokio::fs::write(
        &lock,
        format!("{{\"pid\":{},\"token\":\"other\"}}\n", std::process::id()),
    )
    .await
    .unwrap();
    let error = with_single_file_lock(&file, &quick(), async { Ok(()) })
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::TimedOut);
    // 别人的锁不被我们删掉。
    assert!(lock.exists());
    // 读不出 owner 的锁超过 stale 才回收。
    tokio::fs::write(&lock, "").await.unwrap();
    let options = SingleLockOptions {
        stale: Duration::ZERO,
        ..quick()
    };
    tokio::time::sleep(Duration::from_millis(5)).await;
    with_single_file_lock(&file, &options, async { Ok(()) })
        .await
        .unwrap();
    assert!(!lock.exists());
}

#[tokio::test]
async fn a_cancelled_holder_releases_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("store.json");
    let options = quick();
    let held = with_single_file_lock(&file, &options, std::future::pending::<io::Result<()>>());
    let _ = tokio::time::timeout(Duration::from_millis(30), held).await;
    assert!(!lock_path(&file).exists());
}

/// Node `removeStaleLock`（修复线 F2）：持有者 pid 还活着，但锁里记的 `startTime` 与这个 pid 当前进程的启动时间
/// 相差超过 2 s（pid 已被别的进程复用）时回收；一致或没有 `startTime` 时不回收。pid 1 总是活着。
#[tokio::test]
async fn a_reused_pid_with_another_start_time_is_reclaimed() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("store.json");
    let lock = lock_path(&file);
    let options = SingleLockOptions {
        stale: Duration::ZERO,
        ..quick()
    };
    let Some(started) = super::alive::start_time_ms(1).await else {
        // 平台给不出启动时间（Node 同样不回收）。
        return;
    };
    for content in [
        "{\"pid\":1,\"token\":\"old-format\"}\n".to_owned(),
        format!("{{\"pid\":1,\"startTime\":{started},\"token\":\"same\"}}\n"),
    ] {
        tokio::fs::write(&lock, &content).await.unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
        let error = with_single_file_lock(&file, &options, async { Ok(()) })
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::TimedOut, "{content}");
    }
    let reused = format!(
        "{{\"pid\":1,\"startTime\":{},\"token\":\"reused\"}}\n",
        started + 60_000
    );
    tokio::fs::write(&lock, reused).await.unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    with_single_file_lock(&file, &options, async { Ok(()) })
        .await
        .unwrap();
    assert!(!lock.exists());
}

/// 本工程写的锁带 `startTime`（Node 的键序 `pid, startTime, token`），是本进程的启动时间。
#[tokio::test]
async fn the_lock_records_the_process_start_time() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("store.json");
    let lock = lock_path(&file);
    let seen = with_single_file_lock(&file, &quick(), async {
        tokio::fs::read_to_string(&lock).await
    })
    .await
    .unwrap();
    let Some(started) = super::alive::start_time_ms(std::process::id()).await else {
        return;
    };
    let value: serde_json::Value = serde_json::from_str(&seen).unwrap();
    let keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
    assert_eq!(keys, ["pid", "startTime", "token"]);
    let recorded = value["startTime"].as_u64().unwrap();
    assert!(
        recorded.abs_diff(started) <= 2_000,
        "{recorded} vs {started}"
    );
}
