// 复制自 rust/crates/proc/src/ledger/drop_tests.rs（feat/rs 79b6691c55）。
//! 没有确认就被丢弃的进程树（任务或 runtime 实例被中止）：结束整个进程组；release 时保留。

use std::time::Duration;

use super::{Cleanup, background_of, ended, records, running, sh};
use crate::ledger::{Containment, Isolation, Ledger, Purpose};

#[tokio::test]
async fn dropping_an_unconfirmed_tree_kills_its_whole_group() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(dir.path().into());
    // `kill_on_drop(false)`：结束进程组的是 Tracked 自己，不是根进程的 kill_on_drop。
    let mut command = sh();
    let mut aborted = ledger
        .spawn(
            &mut command,
            Purpose::Bash,
            Isolation::Session,
            Containment::OwnJob,
        )
        .await
        .unwrap();
    let (pid, background) = (
        i32::try_from(aborted.pid()).unwrap(),
        background_of(aborted.child()).await,
    );
    let _cleanup = Cleanup(vec![pid]);

    drop(aborted);

    assert_eq!((ended(pid).await, ended(background).await), (true, true));
    // 记录留到下一次清扫：根进程已不存在，只删除记录。
    assert_eq!(records(dir.path()).len(), 1);
    drop(ledger);
    Ledger::open(dir.path().into()).sweep().await;
    assert_eq!(records(dir.path()), Vec::<String>::new());
}

#[tokio::test]
async fn releasing_a_running_root_keeps_its_group() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(dir.path().into());
    let mut command = sh();
    let mut hook = ledger
        .spawn(
            &mut command,
            Purpose::Hook,
            Isolation::Session,
            Containment::ProcessJob,
        )
        .await
        .unwrap();
    let (pid, background) = (
        i32::try_from(hook.pid()).unwrap(),
        background_of(hook.child()).await,
    );
    let _cleanup = Cleanup(vec![pid]);

    hook.release().await;

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        (running(pid).await, running(background).await),
        (true, true)
    );
}

/// 缺陷 R2-17：丢弃时原来只在写过记录时才结束进程组；账本目录写不进去（记录为空）时，被中止的
/// 任务留下整个进程组。现在用单独的“已确认”标记判断。
#[tokio::test]
async fn dropping_an_unconfirmed_tree_kills_its_group_even_without_a_record() {
    let dir = tempfile::tempdir().unwrap();
    // 账本目录的位置是一个普通文件：写记录失败。
    let blocked = dir.path().join("ledger");
    std::fs::write(&blocked, b"").unwrap();
    let ledger = Ledger::open(blocked);
    let mut command = sh();
    let mut aborted = ledger
        .spawn(
            &mut command,
            Purpose::Bash,
            Isolation::Session,
            Containment::OwnJob,
        )
        .await
        .unwrap();
    let (pid, background) = (
        i32::try_from(aborted.pid()).unwrap(),
        background_of(aborted.child()).await,
    );
    let _cleanup = Cleanup(vec![pid]);

    drop(aborted);

    assert_eq!((ended(pid).await, ended(background).await), (true, true));
}
