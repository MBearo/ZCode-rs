//! `.zcode/scheduled_tasks.json` 的监听（docs/specs/rust-cli/tools-more.md §16.2）。
//!
//! 监听 `.zcode/` 目录（不递归），只关心名字是 `scheduled_tasks.json` 的事件：原子写是“临时文件 + rename”，
//! 临时文件的事件滤掉。notify 的回调在它自己的线程里，经容量 1 的通道交给流：已有一项没取时丢弃新的
//! （它只是“重读一次”的提醒）。目录不在时先建好（抢锁时同样会建它）。
//!
//! 建监听的函数是参数（[`changes_with`]）：上面这些逻辑用注入的事件源测，与系统的监听服务无关（工作线 F71：macOS 的
//! FSEvents 要等系统守护进程 fseventsd，它忙时登记一次要几十秒，tools-more.md §16.2.1）。

use std::any::Any;
use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use notify::{RecursiveMode, Watcher};
use tokio::sync::mpsc;

use crate::cron_store::TASKS_FILE;

/// 交给事件源的回调：事件源每收到一个事件调用一次（在它自己的线程里）。
pub(crate) type Handler = Box<dyn Fn(notify::Result<notify::Event>) + Send + Sync + 'static>;
/// 活着的监听：丢掉它监听就停。
pub(crate) type Watch = Box<dyn Any + Send>;

/// 监听中的状态：监听要活着，回调才继续送事件。
struct Watching {
    _watch: Watch,
    events: mpsc::Receiver<()>,
}

/// 这个事件说的是不是任务文件（临时文件、别的文件与出错的事件都不算）。
fn relevant(event: &notify::Result<notify::Event>) -> bool {
    event.as_ref().is_ok_and(|event| {
        event
            .paths
            .iter()
            .any(|path| path.file_name().is_some_and(|name| name == TASKS_FILE))
    })
}

/// 立即开始监听（同步）：目录不在时失败。
fn watch<S>(dir: &Path, source: &S) -> notify::Result<Watching>
where
    S: Fn(&Path, Handler) -> notify::Result<Watch>,
{
    let (sender, events) = mpsc::channel(1);
    let handler: Handler = Box::new(move |event| {
        if relevant(&event) {
            // 满了说明还有一次重读没做，丢掉这次即可。
            let _ = sender.try_send(());
        }
    });
    Ok(Watching {
        _watch: source(dir, handler)?,
        events,
    })
}

/// 系统的监听服务（notify：macOS 的 FSEvents、Linux 的 inotify、Windows 的 `ReadDirectoryChangesW`）。
fn system(dir: &Path, handler: Handler) -> notify::Result<Watch> {
    let mut watcher = notify::recommended_watcher(handler)?;
    watcher.watch(dir, RecursiveMode::NonRecursive)?;
    Ok(Box::new(watcher))
}

/// 目录不在时先建好再监听。
async fn start<S>(dir: PathBuf, source: &S) -> Option<Watching>
where
    S: Fn(&Path, Handler) -> notify::Result<Watch>,
{
    if let Err(error) = rust_cli_fs::create_dir_all(&dir).await {
        tracing::warn!(%error, dir = %dir.display(), "cannot create the scheduled task directory");
        return None;
    }
    watch(&dir, source)
        .inspect_err(
            |error| tracing::warn!(%error, dir = %dir.display(), "cannot watch scheduled tasks"),
        )
        .ok()
}

/// `dir`（`<项目根>/.zcode`）里任务文件的变化：每次产出一项；监听建不起来时流直接结束。
///
/// 目录已在时立即开始监听（调用方先抢锁，锁文件已经建好目录），这之后的写入都不会漏掉。
pub(crate) fn changes(dir: PathBuf) -> BoxStream<'static, ()> {
    changes_with(dir, system)
}

/// [`changes`]，建监听的函数由调用方给（测试注入事件源）。
pub(crate) fn changes_with<S>(dir: PathBuf, source: S) -> BoxStream<'static, ()>
where
    S: Fn(&Path, Handler) -> notify::Result<Watch> + Send + Sync + 'static,
{
    enum State<S> {
        Pending(PathBuf, S),
        Running(Watching, S),
    }
    let state = match watch(&dir, &source) {
        Ok(watching) => State::Running(watching, source),
        Err(_) => State::Pending(dir, source),
    };
    futures_util::stream::unfold(state, |state| async move {
        let (mut watching, source) = match state {
            State::Pending(dir, source) => (start(dir, &source).await?, source),
            State::Running(watching, source) => (watching, source),
        };
        watching.events.recv().await?;
        Some(((), State::Running(watching, source)))
    })
    .boxed()
}

#[cfg(test)]
#[path = "cron_watch_tests.rs"]
mod tests;
