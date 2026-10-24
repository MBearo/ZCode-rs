//! 独占文件锁：同一时刻只有一个持有者；放开后下一个拿到；等锁的一方在放开时醒来。

use std::time::Duration;

use super::{lock_exclusive, try_lock_exclusive};

#[tokio::test]
async fn only_one_holder_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("x.lock");
    let first = try_lock_exclusive(&path).await.unwrap();
    assert!(first.is_some());
    assert!(try_lock_exclusive(&path).await.unwrap().is_none());
    drop(first);
    assert!(try_lock_exclusive(&path).await.unwrap().is_some());
    // 锁文件留着（删掉会让两边锁到不同的文件）。
    assert!(path.exists());
}

#[tokio::test]
async fn a_waiter_gets_the_lock_when_it_is_released() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("x.lock");
    let held = lock_exclusive(&path).await.unwrap();
    let waiting = rust_cli_base::owned({
        let path = path.clone();
        async move { lock_exclusive(&path).await.map(|_| ()) }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!waiting.is_finished());
    drop(held);
    tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
