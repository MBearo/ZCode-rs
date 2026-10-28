// 复制自 rust/crates/async/src/owned_tests.rs（feat/rs 79b6691c55）。
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::owned::{blocking, owned};
use super::scope::TaskError;

const SOON: Duration = Duration::from_secs(5);

/// 被丢弃时置位：用来观察任务的 future 是否已被释放。
struct Dropped(Arc<AtomicBool>);

impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

async fn until(what: &str, check: impl Fn() -> bool) {
    tokio::time::timeout(SOON, async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

#[tokio::test]
async fn the_result_passes_through_and_a_panic_becomes_an_error() {
    assert_eq!(owned(async { 40 + 2 }).await, Ok(42));
    let failed = owned(async {
        if true {
            panic!("owned boom");
        }
        1
    })
    .await;
    let Err(TaskError::Panicked(panicked)) = failed else {
        panic!("expected a panic, got {failed:?}");
    };
    assert_eq!(panicked.message, "owned boom");
}

#[tokio::test]
async fn dropping_the_handle_aborts_the_task() {
    let dropped = Arc::new(AtomicBool::new(false));
    let guard = Dropped(dropped.clone());
    let task = owned(async move {
        let _guard = guard;
        std::future::pending::<()>().await;
    });
    tokio::task::yield_now().await;
    assert!(!dropped.load(Ordering::SeqCst));
    drop(task);
    until("the task to be dropped", || dropped.load(Ordering::SeqCst)).await;

    let aborted = owned(std::future::pending::<()>());
    aborted.abort();
    assert_eq!(aborted.await, Err(TaskError::Aborted));
}

#[tokio::test]
async fn a_moved_handle_keeps_the_task_alive() {
    let (release, released) = tokio::sync::oneshot::channel::<u8>();
    let task = owned(async move { released.await.unwrap_or(0) });
    // 句柄移进另一个任务：所有权随之转移，原来的作用域结束不影响它。
    let holder = owned(task);
    tokio::task::yield_now().await;
    assert!(!holder.is_finished());
    release.send(7).unwrap();
    assert_eq!(holder.await, Ok(Ok(7)));

    // 新的所有者被中止时，它持有的句柄随之释放，任务也被中止。
    let dropped = Arc::new(AtomicBool::new(false));
    let guard = Dropped(dropped.clone());
    let inner = owned(async move {
        let _guard = guard;
        std::future::pending::<()>().await;
    });
    let holder = owned(inner);
    tokio::task::yield_now().await;
    assert!(!dropped.load(Ordering::SeqCst));
    drop(holder);
    until("the inner task to be dropped", || {
        dropped.load(Ordering::SeqCst)
    })
    .await;
}

#[tokio::test]
async fn blocking_work_returns_its_result_or_its_panic() {
    assert_eq!(blocking(|| 6 * 7).await, Ok(42));
    let failed = blocking(|| -> u8 { panic!("blocking boom") }).await;
    let Err(TaskError::Panicked(panicked)) = failed else {
        panic!("expected a panic, got {failed:?}");
    };
    assert_eq!(panicked.message, "blocking boom");
}

/// 修复线 F109：阻塞调用的 panic 在阻塞线程里转换并清掉那个线程的 panic 记录；以前在等待方的线程上转换，阻塞线程
/// 被复用时记录还在，下一次 panic 会被当成展开中的第二次。
#[test]
fn a_blocking_panic_clears_the_note_of_the_blocking_thread() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let failed = blocking(|| -> u8 {
            crate::note_panic("blocking".to_owned());
            panic!("blocking boom")
        })
        .await;
        assert!(matches!(failed, Err(TaskError::Panicked(_))), "{failed:?}");
        let first = blocking(|| std::thread::current().id()).await.unwrap();
        // 阻塞线程池只有一个线程：两次调用都在它上面。
        let (left, second) = blocking(|| (crate::take_panic_note(), std::thread::current().id()))
            .await
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(left, None);
    });
}
