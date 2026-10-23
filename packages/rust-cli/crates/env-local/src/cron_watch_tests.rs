//! 任务文件的监听（tools-more.md §16.2、§16.2.1，工作线 F71、F73）：注入的事件源，与系统的监听服务无关。
#![expect(
    clippy::disallowed_methods,
    reason = "测试直接准备目录夹具；注入的事件源像系统的监听一样检查目录"
)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc as std_mpsc};
use std::time::Duration;

use futures_util::{FutureExt, StreamExt};
use notify::event::{CreateKind, EventKind, ModifyKind};
use tokio::sync::Notify;

use super::*;

/// 建过的监听：目录与回调。
type Watches = Vec<(PathBuf, Arc<Handler>)>;

/// 卡住的最长时间：到期说明放行它的任务一直没有机会运行。
const STALL: Duration = Duration::from_secs(3);

/// 卡住调用它的线程，像系统的监听服务忙时那样：等别的任务放行，最多等 [`STALL`]。
struct Stall {
    release: Mutex<std_mpsc::Receiver<()>>,
    /// 开始卡住时通知（测试据此知道登记已经开始）。
    waiting: Notify,
    expired: AtomicBool,
}

impl Stall {
    fn new() -> (Arc<Self>, std_mpsc::Sender<()>) {
        let (release, receiver) = std_mpsc::channel();
        let stall = Self {
            release: Mutex::new(receiver),
            waiting: Notify::new(),
            expired: AtomicBool::new(false),
        };
        (Arc::new(stall), release)
    }

    fn wait(&self) {
        self.waiting.notify_one();
        let release = self.release.lock().unwrap_or_else(PoisonError::into_inner);
        if release.recv_timeout(STALL).is_err() {
            self.expired.store(true, Ordering::SeqCst);
        }
    }

    fn expired(&self) -> bool {
        self.expired.load(Ordering::SeqCst)
    }
}

/// 注入的事件源：记下每次建监听的目录与回调；目录不在时与系统的监听一样失败。登记与注销可以卡住。
#[derive(Clone, Default)]
struct Fake {
    watches: Arc<Mutex<Watches>>,
    dropped: Arc<AtomicBool>,
    registered: Arc<Notify>,
    unregistered: Arc<Notify>,
    stall_register: Option<Arc<Stall>>,
    stall_unregister: Option<Arc<Stall>>,
}

/// 活着的监听：丢掉时（注销）先按需卡住，再记下。
struct Alive(Fake);

impl Drop for Alive {
    fn drop(&mut self) {
        if let Some(stall) = &self.0.stall_unregister {
            stall.wait();
        }
        self.0.dropped.store(true, Ordering::SeqCst);
        self.0.unregistered.notify_one();
    }
}

impl Fake {
    fn source(&self) -> impl Fn(&Path, Handler) -> notify::Result<Watch> + Send + Sync + 'static {
        let fake = self.clone();
        move |dir: &Path, handler: Handler| {
            if let Some(stall) = &fake.stall_register {
                stall.wait();
            }
            if !dir.is_dir() {
                return Err(notify::Error::path_not_found());
            }
            fake.lock().push((dir.to_owned(), Arc::new(handler)));
            fake.registered.notify_one();
            Ok(Box::new(Alive(fake.clone())))
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Watches> {
        self.watches.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 像系统那样从回调线程送一个事件。
    fn send(&self, kind: EventKind, path: PathBuf) {
        let handler = self.lock().last().expect("a watch").1.clone();
        handler(Ok(notify::Event::new(kind).add_path(path)));
    }
}

fn modified() -> EventKind {
    EventKind::Modify(ModifyKind::Any)
}

/// 流的下一项（最多等 10 s）。
async fn next(changes: &mut BoxStream<'static, ()>) -> Option<()> {
    tokio::time::timeout(Duration::from_secs(10), changes.next())
        .await
        .expect("the stream moves on")
}

/// 监听建好之后先产出一项（调用方重读，补上登记期间的写入），之后没有变化就不产出。
#[tokio::test]
async fn the_first_item_comes_once_the_watch_is_up() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Fake::default();
    let mut changes = changes_with(dir.path().to_owned(), fake.source());
    assert_eq!(next(&mut changes).await, Some(()), "a reread once watching");
    {
        let watches = fake.lock();
        assert_eq!(watches.len(), 1);
        assert_eq!(watches[0].0, dir.path());
    }
    assert_eq!(changes.next().now_or_never(), None, "nothing else yet");
}

/// 只认任务文件；临时文件、别的文件与出错的事件不算；没取的提醒合成一个。
#[tokio::test]
async fn only_the_tasks_file_counts_and_reminders_coalesce() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Fake::default();
    let mut changes = changes_with(dir.path().to_owned(), fake.source());
    assert_eq!(next(&mut changes).await, Some(()));
    fake.send(
        EventKind::Create(CreateKind::File),
        dir.path().join(".scheduled_tasks.json.tmp-1a2b"),
    );
    fake.send(modified(), dir.path().join("other.json"));
    fake.send(modified(), dir.path().join("scheduled_tasks.lock"));
    let handler = fake.lock()[0].1.clone();
    handler(Err(notify::Error::generic("lost")));
    assert_eq!(changes.next().now_or_never(), None, "nothing relevant yet");

    fake.send(modified(), dir.path().join("scheduled_tasks.json"));
    fake.send(
        EventKind::Create(CreateKind::File),
        dir.path().join("scheduled_tasks.json"),
    );
    assert_eq!(changes.next().now_or_never(), Some(Some(())));
    assert_eq!(
        changes.next().now_or_never(),
        None,
        "two events, one reread"
    );
    fake.send(modified(), dir.path().join("scheduled_tasks.json"));
    assert_eq!(changes.next().now_or_never(), Some(Some(())));
}

/// 目录不在：第一次取流时先建目录再监听，之后的写入产出变化。
#[tokio::test]
async fn a_missing_directory_is_created_before_watching() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join(".zcode");
    let fake = Fake::default();
    let mut changes = changes_with(dir.clone(), fake.source());
    assert!(fake.lock().is_empty() && !dir.exists());
    assert_eq!(next(&mut changes).await, Some(()));
    assert!(dir.is_dir());
    assert_eq!(fake.lock()[0].0, dir);
    fake.send(modified(), dir.join("scheduled_tasks.json"));
    assert_eq!(next(&mut changes).await, Some(()));
}

/// 建不了目录时流直接结束。
#[tokio::test]
async fn a_directory_that_cannot_be_created_ends_the_stream() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("file");
    std::fs::write(&file, b"x").unwrap();
    let fake = Fake::default();
    let mut changes = changes_with(file.join(".zcode"), fake.source());
    assert_eq!(next(&mut changes).await, None);
    assert!(fake.lock().is_empty());
}

/// 建不起监听时流直接结束。
#[tokio::test]
async fn a_failed_registration_ends_the_stream() {
    let dir = tempfile::tempdir().unwrap();
    let refuse =
        |_: &Path, _: Handler| -> notify::Result<Watch> { Err(notify::Error::generic("refused")) };
    let mut changes = changes_with(dir.path().to_owned(), refuse);
    assert_eq!(next(&mut changes).await, None);
}

/// 流活着监听才活着：丢掉流就注销。
#[tokio::test]
async fn the_watch_lives_as_long_as_the_stream() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Fake::default();
    let mut changes = changes_with(dir.path().to_owned(), fake.source());
    assert_eq!(next(&mut changes).await, Some(()));
    assert!(!fake.dropped.load(Ordering::SeqCst));
    drop(changes);
    tokio::time::timeout(Duration::from_secs(10), fake.unregistered.notified())
        .await
        .expect("unregistered after the stream is dropped");
    assert!(fake.dropped.load(Ordering::SeqCst));
}

/// 登记还没完成时丢掉流：登记完成后立即注销。
#[tokio::test]
async fn a_stream_dropped_during_registration_unregisters_once_up() {
    let dir = tempfile::tempdir().unwrap();
    let (stall, release) = Stall::new();
    let fake = Fake {
        stall_register: Some(stall.clone()),
        ..Fake::default()
    };
    let mut changes = changes_with(dir.path().to_owned(), fake.source());
    tokio::select! {
        item = changes.next() => panic!("no item while the registration is stalled: {item:?}"),
        () = stall.waiting.notified() => {}
    }
    drop(changes);
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), fake.unregistered.notified())
        .await
        .expect("unregistered once the watch is up");
    assert_eq!(fake.lock().len(), 1);
    assert!(fake.dropped.load(Ordering::SeqCst));
}

/// 登记卡住（系统的监听服务忙）时，同一个运行时上的别的任务照常推进（单线程运行时：登记要等另一个任务先运行）。
#[tokio::test]
async fn a_stalled_registration_does_not_hold_up_other_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let (stall, release) = Stall::new();
    let fake = Fake {
        stall_register: Some(stall.clone()),
        ..Fake::default()
    };
    let other = tokio::spawn(async move { release.send(()).unwrap() });
    let mut changes = changes_with(dir.path().to_owned(), fake.source());
    assert!(
        !stall.expired(),
        "the registration held up the calling thread"
    );
    assert_eq!(
        next(&mut changes).await,
        Some(()),
        "watching after the other task ran"
    );
    other.await.unwrap();
    assert!(!stall.expired(), "the other task could not run");
}

/// 注销卡住时，丢掉流的线程不等它，别的任务照常推进。
#[tokio::test]
async fn a_stalled_unregistration_does_not_hold_up_other_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let (stall, release) = Stall::new();
    let fake = Fake {
        stall_unregister: Some(stall.clone()),
        ..Fake::default()
    };
    let mut changes = changes_with(dir.path().to_owned(), fake.source());
    tokio::select! {
        biased;
        () = fake.registered.notified() => {}
        item = changes.next() => assert_eq!(item, Some(())),
    }
    let other = tokio::spawn(async move { release.send(()).unwrap() });
    drop(changes);
    assert!(
        !stall.expired(),
        "dropping the stream held up the calling thread"
    );
    tokio::time::timeout(Duration::from_secs(10), fake.unregistered.notified())
        .await
        .expect("unregistered after the other task ran");
    other.await.unwrap();
    assert!(!stall.expired(), "the other task could not run");
}
