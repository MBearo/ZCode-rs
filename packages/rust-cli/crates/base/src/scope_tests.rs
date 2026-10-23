// 复制自 rust/crates/async/src/scope_tests.rs（feat/rs 79b6691c55）。
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::scope::{Spawner, TaskError, TaskPanic, TaskScope};

const SOON: Duration = Duration::from_secs(5);

fn scope() -> (TaskScope, Arc<Mutex<Vec<TaskPanic>>>) {
    let panics = Arc::new(Mutex::new(Vec::new()));
    let sink = panics.clone();
    let scope = TaskScope::root("host", move |panic| {
        sink.lock().unwrap().push(panic.clone());
    });
    (scope, panics)
}

/// 被丢弃时置位：用来观察任务的 future 是否已被释放。
struct Dropped(Arc<AtomicBool>);

impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn forever(spawner: &Spawner, name: &'static str) -> Arc<AtomicBool> {
    let dropped = Arc::new(AtomicBool::new(false));
    let guard = Dropped(dropped.clone());
    spawner.spawn(name, async move {
        let _guard = guard;
        std::future::pending::<()>().await;
    });
    dropped
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
async fn a_panic_is_reported_and_the_waiter_gets_an_error() {
    let (scope, panics) = scope();
    let spawner = scope.spawner();
    let failed = spawner.spawn("explode", async {
        if true {
            panic!("boom");
        }
        1
    });
    let fine = spawner.spawn("add", async { 1 + 1 });
    assert_eq!(fine.await, Ok(2));
    let TaskError::Panicked(panicked) = failed.await.unwrap_err() else {
        panic!("expected a panic");
    };
    assert_eq!(panicked.message, "boom");
    assert_eq!(
        *panics.lock().unwrap(),
        [TaskPanic {
            scope: "host".into(),
            task: "explode",
            message: "boom".into()
        }]
    );
    assert!(spawner.running().is_empty());
}

#[tokio::test]
async fn dropping_the_scope_aborts_its_tasks_and_those_of_child_scopes() {
    let (scope, _) = scope();
    let spawner = scope.spawner();
    let child = spawner.child("connection:1");
    let (own, nested) = (
        forever(&spawner, "own"),
        forever(&child.spawner(), "nested"),
    );
    let mut running = spawner.running();
    running.sort();
    assert_eq!(running, ["host/connection:1: nested", "host: own"]);
    drop(scope);
    until("both tasks to be dropped", || {
        own.load(Ordering::SeqCst) && nested.load(Ordering::SeqCst)
    })
    .await;
    // 作用域被释放后，手里还有 Spawner 也启动不了任务。
    let ran = Arc::new(AtomicBool::new(false));
    let flag = ran.clone();
    let late = spawner.spawn("late", async move { flag.store(true, Ordering::SeqCst) });
    assert_eq!(late.await, Err(TaskError::ScopeClosed));
    assert_eq!(
        child.spawner().spawn("late", async {}).await,
        Err(TaskError::ScopeClosed)
    );
    assert!(!ran.load(Ordering::SeqCst));
    assert!(spawner.is_closed());
}

#[tokio::test]
async fn close_waits_for_cooperative_tasks_and_aborts_the_rest() {
    let (scope, _) = scope();
    let spawner = scope.spawner();
    let finished = Arc::new(AtomicUsize::new(0));
    for _ in 0..3 {
        let (own, done) = (spawner.clone(), finished.clone());
        spawner.spawn("cooperative", async move {
            own.cancelled().await;
            done.fetch_add(1, Ordering::SeqCst);
        });
    }
    assert_eq!(scope.close(SOON).await, Vec::<String>::new());
    assert_eq!(finished.load(Ordering::SeqCst), 3);

    let (scope, _) = self::scope();
    let spawner = scope.spawner();
    let child = spawner.child("remote:a");
    let stubborn = forever(&child.spawner(), "stubborn");
    let own = spawner.clone();
    spawner.spawn("cooperative", async move { own.cancelled().await });
    let left = scope.close(Duration::from_millis(50)).await;
    assert_eq!(left, ["host/remote:a: stubborn"]);
    until("the stubborn task to be dropped", || {
        stubborn.load(Ordering::SeqCst)
    })
    .await;
    assert!(spawner.running().is_empty());
}

#[tokio::test]
async fn a_task_leaves_the_scope_when_it_ends_however_it_ends() {
    let (scope, _) = scope();
    let spawner = scope.spawner();
    let quick = spawner.spawn("quick", async { 7 });
    assert_eq!(quick.await, Ok(7));
    // 启动后立刻中止：future 可能一次都没被轮询。
    let aborted = spawner.spawn("aborted", std::future::pending::<()>());
    aborted.abort();
    assert_eq!(aborted.await, Err(TaskError::Aborted));
    until("the scope to be empty", || spawner.running().is_empty()).await;

    // 子作用域结束后不再占着父作用域的列表：关闭父作用域立即完成。
    for index in 0..100 {
        let child = spawner.child(format!("connection:{index}"));
        child.spawner().spawn("work", async {}).await.unwrap();
    }
    assert_eq!(scope.close(SOON).await, Vec::<String>::new());
}

#[tokio::test]
async fn blocking_work_is_tracked_and_its_panic_reported() {
    let (scope, panics) = scope();
    let spawner = scope.spawner();
    assert_eq!(spawner.spawn_blocking("sum", || 40 + 2).await, Ok(42));
    let failed = spawner
        .spawn_blocking("explode", || -> u8 { panic!("blocking boom") })
        .await;
    assert!(matches!(failed, Err(TaskError::Panicked(_))));
    let reported = panics.lock().unwrap().clone();
    assert_eq!(reported.len(), 1);
    assert_eq!(
        (reported[0].task, reported[0].message.as_str()),
        ("explode", "blocking boom")
    );
    assert!(spawner.running().is_empty());
}

#[tokio::test]
async fn a_child_created_after_close_is_already_closed() {
    let (scope, _) = scope();
    let spawner = scope.spawner();
    scope.close(SOON).await;
    let child = spawner.child("late");
    assert!(child.spawner().is_closed());
    assert_eq!(
        child.spawner().spawn("work", async {}).await,
        Err(TaskError::ScopeClosed)
    );
}

/// 常驻任务随作用域的取消信号结束：`close` 不把它列为被中止，父作用域的取消同样传到子作用域；
/// 自己先结束时句柄得到结果。
#[tokio::test]
async fn resident_tasks_end_on_the_cancel_signal_and_are_not_reported_as_aborted() {
    let (scope, _) = scope();
    let spawner = scope.spawner();
    let child = spawner.child("services/agent");
    let dropped = Arc::new(AtomicBool::new(false));
    let guard = Dropped(dropped.clone());
    let resident = child.spawner().spawn_resident("poll", async move {
        let _guard = guard;
        std::future::pending::<()>().await;
    });
    let finished = spawner.spawn_resident("quick", async { 7 });
    assert_eq!(finished.await, Ok(Some(7)));
    assert_eq!(scope.close(SOON).await, Vec::<String>::new());
    assert_eq!(resident.await, Ok(None));
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(
        spawner.spawn_resident("late", async {}).await,
        Err(TaskError::ScopeClosed)
    );
}
