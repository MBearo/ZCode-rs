//! `.zcode/scheduled_tasks.json` 的监听（docs/specs/rust-cli/tools-more.md §16.2）。
//!
//! 监听 `.zcode/` 目录（不递归），只关心名字是 `scheduled_tasks.json` 的事件：原子写是“临时文件 + rename”，
//! 临时文件的事件滤掉。notify 的回调在它自己的线程里，经容量 1 的通道交给流：已有一项没取时丢弃新的
//! （它只是“重读一次”的提醒）。目录不在时先建好（抢锁时同样会建它）。
//!
//! 登记与注销在阻塞线程上（工作线 F73，tools-more.md §16.2.1 第 4 条）。修 bug：以前 `changes()` 在调用方的 tokio
//! 工作线程上同步登记、流被丢掉时在同一类线程上同步注销；macOS 的 FSEvents 要等系统守护进程 fseventsd，它忙时一次要
//! 几十秒，线程上的别的任务（会话 actor、回合）跟着停。现在一个阻塞线程登记、持有并注销监听；登记完成后流先产出一项，
//! 调用方重读一次，补上登记期间别处的写入。
//!
//! 建监听的函数是参数（[`changes_with`]）：上面这些逻辑用注入的事件源测，与系统的监听服务无关（工作线 F71）。

use std::any::Any;
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::mpsc as std_mpsc;
use std::task::{Context, Poll};

use futures_util::future::BoxFuture;
use futures_util::stream::{BoxStream, Stream};
use futures_util::{FutureExt, StreamExt};
use notify::{RecursiveMode, Watcher};
use tokio::sync::mpsc;

use crate::cron_store::TASKS_FILE;

/// 交给事件源的回调：事件源每收到一个事件调用一次（在它自己的线程里）。
pub(crate) type Handler = Box<dyn Fn(notify::Result<notify::Event>) + Send + Sync + 'static>;
/// 活着的监听：丢掉它监听就停（注销）。
pub(crate) type Watch = Box<dyn Any + Send>;

/// 这个事件说的是不是任务文件（临时文件、别的文件与出错的事件都不算）。
fn relevant(event: &notify::Result<notify::Event>) -> bool {
    event.as_ref().is_ok_and(|event| {
        event
            .paths
            .iter()
            .any(|path| path.file_name().is_some_and(|name| name == TASKS_FILE))
    })
}

/// 系统的监听服务（notify：macOS 的 FSEvents、Linux 的 inotify、Windows 的 `ReadDirectoryChangesW`）。
fn system(dir: &Path, handler: Handler) -> notify::Result<Watch> {
    let mut watcher = notify::recommended_watcher(handler)?;
    watcher.watch(dir, RecursiveMode::NonRecursive)?;
    Ok(Box::new(watcher))
}

/// 在阻塞线程上运行：登记，发第一项（重读），持有监听直到流被丢掉，再在这个线程上注销。
///
/// 登记失败时回调与 `ready` 一起释放，通道关闭，流结束。
fn hold<S>(
    dir: &Path,
    source: S,
    handler: Handler,
    ready: mpsc::Sender<()>,
    stopped: std_mpsc::Receiver<Infallible>,
) where
    S: FnOnce(&Path, Handler) -> notify::Result<Watch>,
{
    let watch = match source(dir, handler) {
        Ok(watch) => watch,
        Err(error) => {
            tracing::warn!(%error, dir = %dir.display(), "cannot watch scheduled tasks");
            return;
        }
    };
    // 满了说明已经排着一次重读，同样补得上登记期间的写入。
    let _ = ready.try_send(());
    // 流被丢掉时发送端释放，`recv` 返回错误（通道里不会有值）；之后注销。
    let _ = stopped.recv();
    drop(watch);
    drop(ready);
}

/// [`changes_with`] 的流：每次轮询先推进建监听的那一步（第一次轮询时开始），再取通道里的提醒。
struct Changes {
    /// 建目录，再在阻塞线程上登记并持有监听；丢掉它只是不再等那个线程。
    start: Option<BoxFuture<'static, ()>>,
    events: mpsc::Receiver<()>,
    /// 随流一起释放：持有监听的线程据此注销。
    _stop: std_mpsc::Sender<Infallible>,
}

impl Stream for Changes {
    type Item = ();

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<()>> {
        if let Some(start) = &mut self.start
            && start.poll_unpin(context).is_ready()
        {
            // 只在建目录或登记失败（或运行时正在关闭）时结束：发送端都已释放，下面取到 None。
            self.start = None;
        }
        self.events.poll_recv(context)
    }
}

/// `dir`（`<项目根>/.zcode`）里任务文件的变化：监听建好时先产出一项（重读），之后每次变化产出一项；监听建不起来时
/// 流直接结束。第一次取流时才开始建监听。
pub(crate) fn changes(dir: PathBuf) -> BoxStream<'static, ()> {
    changes_with(dir, system)
}

/// [`changes`]，建监听的函数由调用方给（测试注入事件源）。
pub(crate) fn changes_with<S>(dir: PathBuf, source: S) -> BoxStream<'static, ()>
where
    S: FnOnce(&Path, Handler) -> notify::Result<Watch> + Send + 'static,
{
    let (sender, events) = mpsc::channel(1);
    let ready = sender.clone();
    let handler: Handler = Box::new(move |event| {
        if relevant(&event) {
            // 满了说明还有一次重读没做，丢掉这次即可。
            let _ = sender.try_send(());
        }
    });
    let (stop, stopped) = std_mpsc::channel();
    let start = async move {
        if let Err(error) = rust_cli_fs::create_dir_all(&dir).await {
            tracing::warn!(%error, dir = %dir.display(), "cannot create the scheduled task directory");
            return;
        }
        // 线程持有监听直到流被丢掉；这里等它只为在登记失败时结束流（结果在通道上体现）。
        let _ = rust_cli_base::blocking(move || hold(&dir, source, handler, ready, stopped)).await;
    }
    .boxed();
    Changes {
        start: Some(start),
        events,
        _stop: stop,
    }
    .boxed()
}

#[cfg(test)]
#[path = "cron_watch_tests.rs"]
mod tests;
