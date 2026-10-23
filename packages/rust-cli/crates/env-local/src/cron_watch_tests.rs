//! 任务文件的监听（tools-more.md §16.2、§16.2.1，工作线 F71）：注入的事件源，与系统的监听服务无关。
#![expect(
    clippy::disallowed_methods,
    reason = "测试直接准备目录夹具；注入的事件源像系统的监听一样检查目录"
)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::{FutureExt, StreamExt};
use notify::event::{CreateKind, EventKind, ModifyKind};

use super::*;

/// 建过的监听：目录与回调。
type Watches = Vec<(PathBuf, Arc<Handler>)>;

/// 注入的事件源：记下每次建监听的目录与回调；目录不在时与系统的监听一样失败。
#[derive(Clone, Default)]
struct Fake {
    watches: Arc<Mutex<Watches>>,
    dropped: Arc<AtomicBool>,
    registered: Arc<tokio::sync::Notify>,
}

/// 监听活着的标记：丢掉时记下。
struct Alive(Arc<AtomicBool>);

impl Drop for Alive {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl Fake {
    fn source(&self) -> impl Fn(&Path, Handler) -> notify::Result<Watch> + Send + Sync + 'static {
        let fake = self.clone();
        move |dir: &Path, handler: Handler| {
            if !dir.is_dir() {
                return Err(notify::Error::path_not_found());
            }
            fake.lock().push((dir.to_owned(), Arc::new(handler)));
            fake.registered.notify_one();
            Ok(Box::new(Alive(fake.dropped.clone())))
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

/// 目录已在：`changes()` 返回之前就开始监听，之后的写入不会漏掉。
#[test]
fn an_existing_directory_is_watched_before_changes_returns() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Fake::default();
    let _changes = changes_with(dir.path().to_owned(), fake.source());
    let watches = fake.lock();
    assert_eq!(watches.len(), 1);
    assert_eq!(watches[0].0, dir.path());
}

/// 只认任务文件；临时文件、别的文件与出错的事件不算；没取的提醒合成一个。
#[tokio::test]
async fn only_the_tasks_file_counts_and_reminders_coalesce() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Fake::default();
    let mut changes = changes_with(dir.path().to_owned(), fake.source());
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
    {
        let next = changes.next();
        tokio::select! {
            item = next => panic!("no change yet: {item:?}"),
            () = fake.registered.notified() => {}
        }
    }
    assert!(dir.is_dir());
    assert_eq!(fake.lock()[0].0, dir);
    fake.send(modified(), dir.join("scheduled_tasks.json"));
    let next = tokio::time::timeout(Duration::from_secs(10), changes.next()).await;
    assert_eq!(next.unwrap(), Some(()));
}

/// 建不了目录时流直接结束。
#[tokio::test]
async fn a_directory_that_cannot_be_created_ends_the_stream() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("file");
    std::fs::write(&file, b"x").unwrap();
    let fake = Fake::default();
    let mut changes = changes_with(file.join(".zcode"), fake.source());
    assert_eq!(changes.next().await, None);
    assert!(fake.lock().is_empty());
}

/// 流活着监听才活着：丢掉流就停止监听。
#[test]
fn the_watch_lives_as_long_as_the_stream() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Fake::default();
    let changes = changes_with(dir.path().to_owned(), fake.source());
    assert!(!fake.dropped.load(Ordering::SeqCst));
    drop(changes);
    assert!(fake.dropped.load(Ordering::SeqCst));
}
