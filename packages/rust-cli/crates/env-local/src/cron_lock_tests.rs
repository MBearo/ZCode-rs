use super::*;
use crate::cron_store::Identity;

fn me() -> Identity {
    Identity {
        pid: std::process::id(),
        started: Some("Sat Oct 18 01:02:03 2026".to_owned()),
    }
}

fn path(dir: &tempfile::TempDir) -> std::path::PathBuf {
    dir.path().join(".zcode").join("scheduled_tasks.lock")
}

async fn written(path: &Path) -> LockRecord {
    let bytes = rust_cli_fs::read_optional(path).await.unwrap().unwrap();
    LockRecord::parse(&String::from_utf8(bytes).unwrap()).unwrap()
}

/// 退出了的子进程的 PID（短时间内不会被复用）。
#[cfg(unix)]
async fn dead_pid() -> u32 {
    let mut child = rust_cli_proc::command("true").spawn().unwrap();
    let pid = child.id().unwrap();
    child.wait().await.unwrap();
    pid
}

#[test]
fn records_use_the_lock_file_shape() {
    let record = LockRecord {
        holder: "sess_a".to_owned(),
        pid: 42,
        proc_start: Some("Sat Oct 18 01:02:03 2026".to_owned()),
        acquired_at: 7,
    };
    assert_eq!(
        record.render(),
        r#"{"sessionId":"sess_a","pid":42,"procStart":"Sat Oct 18 01:02:03 2026","acquiredAt":7}"#
    );
    assert_eq!(LockRecord::parse(&record.render()), Some(record));
    assert_eq!(
        LockRecord::parse(r#"{"sessionId":"s","pid":1,"acquiredAt":2}"#).map(|r| r.proc_start),
        Some(None)
    );
    for broken in ["", "{}", r#"{"sessionId":1,"pid":1,"acquiredAt":2}"#] {
        assert_eq!(LockRecord::parse(broken), None, "{broken}");
    }
}

/// 第一次创建（连目录一起）；同一个持有者再抢仍是自己的；另一个会话在活着的进程里持锁时让出。
#[tokio::test]
async fn exclusive_create_and_live_holders() {
    let dir = tempfile::tempdir().unwrap();
    let lock = path(&dir);
    assert!(acquire(&lock, "sess_a", &me(), 1).await.unwrap());
    assert_eq!(written(&lock).await.holder, "sess_a");
    assert!(acquire(&lock, "sess_a", &me(), 2).await.unwrap());
    assert_eq!(
        written(&lock).await.acquired_at,
        1,
        "re-acquiring does not rewrite"
    );
    // 同一个进程（PID 存活、启动时间未知）里的另一个会话：不抢。
    let other = Identity {
        pid: std::process::id(),
        started: None,
    };
    let mut held = written(&lock).await;
    held.proc_start = None;
    rust_cli_fs::atomic_write(
        &lock,
        held.render().as_bytes(),
        crate::cron_store::write_options(),
    )
    .await
    .unwrap();
    assert!(!acquire(&lock, "sess_b", &other, 3).await.unwrap());
    assert_eq!(written(&lock).await.holder, "sess_a");
}

/// resume 后会话 ID 相同、PID 不同：改写成本进程。
#[cfg(unix)]
#[tokio::test]
async fn the_same_holder_in_a_new_process_rewrites_the_pid() {
    let dir = tempfile::tempdir().unwrap();
    let lock = path(&dir);
    let old = Identity {
        pid: dead_pid().await,
        started: None,
    };
    assert!(acquire(&lock, "sess_a", &old, 1).await.unwrap());
    assert!(acquire(&lock, "sess_a", &me(), 2).await.unwrap());
    let record = written(&lock).await;
    assert_eq!((record.pid, record.acquired_at), (std::process::id(), 2));
}

/// 失效的锁回收：持有进程已退出、PID 被复用（启动时间不同）、坏文件。
#[cfg(unix)]
#[tokio::test]
async fn stale_locks_are_recovered() {
    let dir = tempfile::tempdir().unwrap();
    let lock = path(&dir);
    let gone = Identity {
        pid: dead_pid().await,
        started: Some("Thu Jan  1 00:00:00 1970".to_owned()),
    };
    assert!(acquire(&lock, "sess_old", &gone, 1).await.unwrap());
    assert!(acquire(&lock, "sess_a", &me(), 2).await.unwrap());
    assert_eq!(written(&lock).await.holder, "sess_a");

    // 记录的是本进程的 PID，但启动时间对不上：原来的持有者早已退出、PID 被复用。
    let reused = LockRecord {
        holder: "sess_reused".to_owned(),
        pid: std::process::id(),
        proc_start: Some("Thu Jan  1 00:00:00 1970".to_owned()),
        acquired_at: 3,
    };
    rust_cli_fs::atomic_write(
        &lock,
        reused.render().as_bytes(),
        crate::cron_store::write_options(),
    )
    .await
    .unwrap();
    assert!(acquire(&lock, "sess_b", &me(), 4).await.unwrap());
    assert_eq!(written(&lock).await.holder, "sess_b");

    rust_cli_fs::atomic_write(&lock, b"not json", crate::cron_store::write_options())
        .await
        .unwrap();
    assert!(acquire(&lock, "sess_c", &me(), 5).await.unwrap());
    assert_eq!(written(&lock).await.holder, "sess_c");
}

#[tokio::test]
async fn release_only_removes_our_own_lock() {
    let dir = tempfile::tempdir().unwrap();
    let lock = path(&dir);
    assert!(acquire(&lock, "sess_a", &me(), 1).await.unwrap());
    release(&lock, "sess_b").await.unwrap();
    assert_eq!(written(&lock).await.holder, "sess_a");
    release(&lock, "sess_a").await.unwrap();
    assert_eq!(rust_cli_fs::read_optional(&lock).await.unwrap(), None);
    release(&lock, "sess_a").await.unwrap();
}
