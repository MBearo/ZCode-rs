//! 文件版账本（remote-exec.md §19.7）：登记 / 完成 / 取回；重开后只有登记的记成中断；只读与流不进日志；写到一半的
//! 最后一行；过期变墓碑、墓碑删除、大小上限、压缩；身份不变。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use rust_cli_exec_server::{Begun, CallKey, CallKind, ExecLedger};
use rust_cli_kernel_api::clock::Clock;

use super::{FileLedger, Retention};

/// 测试的时钟：测试自己拨。
#[derive(Default)]
struct TestClock(AtomicU64);

impl TestClock {
    fn advance(&self, by: Duration) {
        self.0
            .fetch_add(u64::try_from(by.as_millis()).unwrap(), Ordering::SeqCst);
    }
}

impl Clock for TestClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }

    fn time_zone(&self) -> String {
        "UTC".to_owned()
    }
}

fn key(call: u64) -> CallKey {
    CallKey {
        instance: "client-1".to_owned(),
        call,
    }
}

async fn open(dir: &std::path::Path, clock: &Arc<TestClock>, retention: Retention) -> FileLedger {
    FileLedger::open(dir, clock.clone(), retention)
        .await
        .unwrap()
}

#[tokio::test]
async fn mutations_survive_a_restart_and_unfinished_ones_become_interrupted() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::default());
    let ledger = open(dir.path(), &clock, Retention::default()).await;
    let identity = ledger.identity().unwrap();
    assert_eq!(
        ledger
            .begin(&key(1), "fp1", CallKind::Mutation)
            .await
            .unwrap(),
        Begun::New
    );
    assert_eq!(
        ledger
            .begin(&key(2), "fp2", CallKind::Mutation)
            .await
            .unwrap(),
        Begun::New
    );
    ledger
        .finish(&key(1), Bytes::from_static(b"reply-1"))
        .await
        .unwrap();
    assert_eq!(
        ledger
            .begin(&key(1), "fp1", CallKind::Mutation)
            .await
            .unwrap(),
        Begun::Done(Bytes::from_static(b"reply-1"))
    );
    assert_eq!(
        ledger
            .begin(&key(1), "other", CallKind::Mutation)
            .await
            .unwrap(),
        Begun::Conflict
    );
    drop(ledger);

    // 常驻进程重启：同一份账本、同一个身份；没有终态的 2 记成中断，完成的 1 仍能取回。
    let again = open(dir.path(), &clock, Retention::default()).await;
    assert_eq!(again.identity().unwrap(), identity);
    assert_eq!(again.recover().await.unwrap(), [key(2)]);
    assert_eq!(
        again
            .begin(&key(2), "fp2", CallKind::Mutation)
            .await
            .unwrap(),
        Begun::Interrupted
    );
    assert_eq!(
        again
            .begin(&key(1), "fp1", CallKind::Mutation)
            .await
            .unwrap(),
        Begun::Done(Bytes::from_static(b"reply-1"))
    );
    // 没登记过的照常执行；ack 之后删掉。
    assert_eq!(
        again
            .begin(&key(3), "fp3", CallKind::Mutation)
            .await
            .unwrap(),
        Begun::New
    );
    again.forget(&key(1)).await.unwrap();
    drop(again);
    let third = open(dir.path(), &clock, Retention::default()).await;
    assert_eq!(third.recover().await.unwrap(), [key(3)]);
    assert_eq!(
        third
            .begin(&key(1), "fp1", CallKind::Mutation)
            .await
            .unwrap(),
        Begun::New
    );
}

#[tokio::test]
async fn reads_and_streams_stay_in_memory() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::default());
    let ledger = open(dir.path(), &clock, Retention::default()).await;
    assert_eq!(
        ledger.begin(&key(1), "r", CallKind::Read).await.unwrap(),
        Begun::New
    );
    assert_eq!(
        ledger.begin(&key(2), "s", CallKind::Stream).await.unwrap(),
        Begun::New
    );
    ledger
        .append_event(&key(2), 1, Bytes::from_static(b"e1"))
        .await
        .unwrap();
    ledger
        .append_event(&key(2), 2, Bytes::from_static(b"e2"))
        .await
        .unwrap();
    ledger.ack_events(&key(2), 1).await.unwrap();
    assert_eq!(
        ledger.events_after(&key(2), 0).await.unwrap(),
        [Bytes::from_static(b"e2")]
    );
    ledger
        .finish(&key(1), Bytes::from_static(b"read"))
        .await
        .unwrap();
    assert_eq!(ledger.durable_len().await, 0);
    assert_eq!(ledger.journal_bytes().await, 0);
    // 断开太久的实例：内存里的记录删掉。
    ledger.forget_client("client-1").await.unwrap();
    assert_eq!(
        ledger.begin(&key(1), "r", CallKind::Read).await.unwrap(),
        Begun::New
    );
    drop(ledger);
    let again = open(dir.path(), &clock, Retention::default()).await;
    assert!(again.recover().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_half_written_last_line_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::default());
    let ledger = open(dir.path(), &clock, Retention::default()).await;
    ledger
        .begin(&key(1), "fp", CallKind::Mutation)
        .await
        .unwrap();
    drop(ledger);
    let journal = dir.path().join("journal");
    let mut text = rust_cli_fs::read_optional(&journal).await.unwrap().unwrap();
    text.extend_from_slice(b"{\"t\":\"b\",\"i\":\"client-1\",\"c\":2,\"f\":\"fp");
    rust_cli_fs::atomic_write(
        &journal,
        &text,
        rust_cli_fs::WriteOptions {
            mode: rust_cli_fs::FileMode::Private,
            temp_name: rust_cli_fs::TempName::Hidden,
            parent: rust_cli_fs::ParentDir::Existing,
            rename_retry: &[],
        },
    )
    .await
    .unwrap();
    let again = open(dir.path(), &clock, Retention::default()).await;
    assert_eq!(again.recover().await.unwrap(), [key(1)]);
}

#[tokio::test]
async fn finished_replies_expire_into_tombstones_that_are_removed_later() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::default());
    let ledger = open(dir.path(), &clock, Retention::default()).await;
    ledger
        .begin(&key(1), "fp", CallKind::Mutation)
        .await
        .unwrap();
    ledger
        .finish(&key(1), Bytes::from_static(b"r"))
        .await
        .unwrap();
    clock.advance(Duration::from_secs(23 * 60 * 60));
    assert_eq!(ledger.sweep().await.unwrap(), (0, 0));
    clock.advance(Duration::from_secs(60 * 60));
    assert_eq!(ledger.sweep().await.unwrap(), (1, 0));
    // 墓碑：结果不明，不重跑。
    assert_eq!(
        ledger
            .begin(&key(1), "fp", CallKind::Mutation)
            .await
            .unwrap(),
        Begun::Interrupted
    );
    clock.advance(Duration::from_secs(7 * 24 * 60 * 60));
    assert_eq!(ledger.sweep().await.unwrap(), (0, 1));
    assert_eq!(ledger.durable_len().await, 0);
    // 墓碑删掉之后同一个调用 ID 就是新调用。
    assert_eq!(
        ledger
            .begin(&key(1), "fp", CallKind::Mutation)
            .await
            .unwrap(),
        Begun::New
    );
}

#[tokio::test]
async fn over_the_size_limit_the_oldest_replies_become_tombstones_and_the_log_is_compacted() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::default());
    let retention = Retention {
        max_bytes: 4000,
        target_bytes: 2000,
        compact_min: 1000,
        ..Retention::default()
    };
    let ledger = open(dir.path(), &clock, retention).await;
    let reply = Bytes::from(vec![b'x'; 600]);
    for call in 1..=8 {
        clock.advance(Duration::from_secs(1));
        ledger
            .begin(&key(call), "fp", CallKind::Mutation)
            .await
            .unwrap();
        ledger.finish(&key(call), reply.clone()).await.unwrap();
    }
    // 最早的几个变成墓碑，最新的还在。
    assert_eq!(
        ledger
            .begin(&key(1), "fp", CallKind::Mutation)
            .await
            .unwrap(),
        Begun::Interrupted
    );
    assert_eq!(
        ledger
            .begin(&key(8), "fp", CallKind::Mutation)
            .await
            .unwrap(),
        Begun::Done(reply.clone())
    );
    // 压缩过：日志比全部写过的记录小得多；重开后结论相同。
    assert!(
        ledger.journal_bytes().await < 8 * 800,
        "{}",
        ledger.journal_bytes().await
    );
    drop(ledger);
    let again = open(dir.path(), &clock, retention).await;
    assert!(again.recover().await.unwrap().is_empty());
    assert_eq!(
        again
            .begin(&key(1), "fp", CallKind::Mutation)
            .await
            .unwrap(),
        Begun::Interrupted
    );
    assert_eq!(
        again
            .begin(&key(8), "fp", CallKind::Mutation)
            .await
            .unwrap(),
        Begun::Done(reply)
    );
}
