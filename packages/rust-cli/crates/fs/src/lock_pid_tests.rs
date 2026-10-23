#![allow(
    clippy::disallowed_methods,
    reason = "测试起一个子进程拿到已退出的 pid、把锁文件的 mtime 改到 6 分钟前（fs 只在 crate 级放开了文件类）"
)]
//! Node 的 pid 锁文件（platform.md §10，工作线 F52）。
use std::time::{Duration, SystemTime};

use super::*;

fn quick() -> PidLockOptions {
    PidLockOptions {
        retry: Duration::from_millis(5),
        attempts: 20,
        stale: Duration::from_secs(5 * 60),
    }
}

fn owner_text(pid: u32) -> String {
    format!("{{\"createdAt\":1,\"pid\":{pid}}}")
}

/// 一个已经退出的进程的 pid。
fn dead_pid() -> u32 {
    let mut child = std::process::Command::new("sh")
        .args(["-c", "exit 0"])
        .spawn()
        .unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

#[tokio::test]
async fn the_lock_holds_node_content_and_is_removed_after_the_operation() {
    let dir = tempfile::tempdir().unwrap();
    // 父目录不存在时创建（Node 先 mkdir -p）。
    let lock = dir.path().join("v2").join("telemetry-state.lock");
    let seen = with_pid_lock(&lock, &quick(), async {
        tokio::fs::read_to_string(&lock).await
    })
    .await
    .unwrap();
    let value: Value = serde_json::from_str(&seen).unwrap();
    // Node 的键序 `createdAt, pid`，没有换行。
    assert!(seen.starts_with("{\"createdAt\":"), "{seen}");
    assert!(
        seen.ends_with(&format!(",\"pid\":{}}}", std::process::id())),
        "{seen}"
    );
    assert!(value["createdAt"].as_u64().unwrap() > 1_600_000_000_000);
    assert!(!lock.exists());
    // 操作失败时同样释放。
    let failed: io::Result<()> =
        with_pid_lock(&lock, &quick(), async { Err(io::Error::other("boom")) }).await;
    assert_eq!(failed.unwrap_err().to_string(), "boom");
    assert!(!lock.exists());
}

#[tokio::test]
async fn a_live_fresh_owner_makes_waiters_time_out() {
    let dir = tempfile::tempdir().unwrap();
    let lock = dir.path().join("telemetry-state.lock");
    std::fs::write(&lock, owner_text(std::process::id())).unwrap();
    let error = with_pid_lock(&lock, &quick(), async { Ok(()) })
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::TimedOut);
    assert!(
        error
            .to_string()
            .starts_with("CLI telemetry state lock timeout"),
        "{error}"
    );
    // 不删活着的持有者的锁。
    assert_eq!(
        std::fs::read_to_string(&lock).unwrap(),
        owner_text(std::process::id())
    );
}

#[tokio::test]
async fn an_unreadable_fresh_owner_is_waited_for() {
    let dir = tempfile::tempdir().unwrap();
    let lock = dir.path().join("telemetry-state.lock");
    std::fs::write(&lock, "{\"pid\":1}").unwrap();
    let error = with_pid_lock(&lock, &quick(), async { Ok(()) })
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::TimedOut);
    assert!(lock.exists());
}

#[tokio::test]
async fn a_dead_owner_is_reclaimed() {
    let dir = tempfile::tempdir().unwrap();
    let lock = dir.path().join("telemetry-state.lock");
    for owner in [
        owner_text(dead_pid()),
        "{\"createdAt\":1,\"pid\":0}".to_owned(),
    ] {
        std::fs::write(&lock, &owner).unwrap();
        let ran = with_pid_lock(&lock, &quick(), async { Ok(true) })
            .await
            .unwrap();
        assert!(ran, "{owner}");
        assert!(!lock.exists());
    }
}

#[tokio::test]
async fn a_stale_lock_is_reclaimed_even_with_a_live_owner() {
    let dir = tempfile::tempdir().unwrap();
    let lock = dir.path().join("telemetry-state.lock");
    for owner in [owner_text(std::process::id()), "garbage".to_owned()] {
        std::fs::write(&lock, &owner).unwrap();
        let old = SystemTime::now() - Duration::from_secs(6 * 60);
        std::fs::File::options()
            .write(true)
            .open(&lock)
            .unwrap()
            .set_modified(old)
            .unwrap();
        with_pid_lock(&lock, &quick(), async { Ok(()) })
            .await
            .unwrap();
        assert!(!lock.exists(), "{owner}");
    }
}

/// 持锁的 future 被丢弃（请求取消）时也删除锁，本进程之后的请求不会等自己。
#[tokio::test]
async fn a_dropped_holder_releases_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let lock = dir.path().join("telemetry-state.lock");
    let (entered, inside) = tokio::sync::oneshot::channel();
    let held = {
        let lock = lock.clone();
        async move {
            with_pid_lock(&lock, &quick(), async {
                let _ = entered.send(());
                std::future::pending::<io::Result<()>>().await
            })
            .await
        }
    };
    tokio::select! {
        _ = held => unreachable!("the operation never finishes"),
        _ = inside => {}
    }
    assert!(!lock.exists());
}
