//! 模型轨迹的写者（docs/specs/rust-cli/model-io.md §3.1、§6）：模式、有界队列与唯一的写者任务。
//!
//! model 的记录点只交记录（[`ModelIoSink::record`] 只做 `try_send`）；这里的写者在自己的任务里转成 JSON、压缩、轮转、
//! 写盘（`rust_cli_fs`），不阻塞主循环与流式。写失败只吞掉（Node 相同：诊断不能打断执行路径）。

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use futures_util::future::{BoxFuture, FutureExt};
use rust_cli_base::TaskScope;
use rust_cli_base::secret::EnvSnapshot;
use rust_cli_model::{ModelIoRecord, ModelIoSink, ProviderModel};
use rust_cli_paths::StoragePaths;
use rust_cli_platform::Settings;
use serde_json::{Map, Value};
use tokio::sync::{mpsc, oneshot};

use crate::model_io_files::Files;

/// 选模式的环境变量（Node 的名字与取值，config.md §2.5）。
const RUNTIME_ENV: &str = "ZCODE_RUNTIME_ENV";
/// 队列容量：满了丢这一条并计数（model-io.md §6）。
const CAPACITY: usize = 64;

/// 写不写、写到哪个目录（model-io.md §3.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// 测试环境：不写。
    Off,
    /// 开发：`cli/debug`，不轮转，单文件 256 MiB。
    Development,
    /// 生产：`cli/rollout`，最多 3 个文件，单文件 64 MiB。
    Production,
}

/// `ZCODE_RUNTIME_ENV`（去首尾空白、不分大小写，Node `normalizeZCodeRuntimeEnv`）；其他值或没有时按构建。
pub(crate) fn mode(value: Option<&str>, debug_build: bool) -> Mode {
    match value
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("test") => Mode::Off,
        Some("development") => Mode::Development,
        Some("production") => Mode::Production,
        _ if debug_build => Mode::Development,
        _ => Mode::Production,
    }
}

/// 写一条之前读的全量保留开关（App 的 `modelIoFullRetentionEnabled`，model-io.md §3.4）。
pub(crate) type Retention = Arc<dyn Fn() -> BoxFuture<'static, bool> + Send + Sync>;

/// 队列里的一条记录：文件按会话分，JSON 在写者的阻塞线程上才拼。
pub(crate) struct Pending {
    pub(crate) session: Option<String>,
    pub(crate) started_ms: u64,
    pub(crate) build: Box<dyn FnOnce() -> Map<String, Value> + Send>,
}

impl From<ModelIoRecord> for Pending {
    fn from(record: ModelIoRecord) -> Self {
        Self {
            session: record.session().map(str::to_owned),
            started_ms: record.started_ms(),
            build: Box::new(move || record.into_json()),
        }
    }
}

pub(crate) enum Item {
    Record(Pending),
    /// 写者写完它之前的所有记录后回复。
    Flush(oneshot::Sender<()>),
}

/// 会话组装时打开写者要的输入。
pub(crate) struct Target<'a> {
    pub(crate) env: &'a EnvSnapshot,
    pub(crate) paths: &'a StoragePaths,
    /// 合并后的 `storage.dir`（config.md §2.3）。
    pub(crate) storage_dir: Option<&'a str>,
    /// 相对的 `storage.dir` 的基准目录（工作区根，同插件存储）。
    pub(crate) base: &'a Path,
    pub(crate) settings: Settings,
}

/// 进程的模型轨迹：入口开始时建（那时还不知道目录），会话组装时打开写者，退出前等它写完。
#[derive(Default)]
pub(crate) struct ModelIo {
    writer: OnceLock<Writer>,
}

struct Writer {
    _scope: TaskScope,
    sender: mpsc::Sender<Item>,
    dropped: Arc<AtomicU64>,
}

impl ModelIo {
    /// 按模式给模型端口接上记录方；测试环境原样返回。
    pub(crate) fn attach(&self, model: ProviderModel, target: Target<'_>) -> ProviderModel {
        let development = match mode(target.env.get(RUNTIME_ENV), cfg!(debug_assertions)) {
            Mode::Off => return model,
            Mode::Development => true,
            Mode::Production => false,
        };
        let dir = target
            .paths
            .model_io(target.storage_dir, target.base, development);
        let settings = target.settings;
        let retention: Retention = Arc::new(move || {
            let settings = settings.clone();
            async move { settings.model_io_full_retention().await }.boxed()
        });
        let files = Files::new(dir.as_path().to_path_buf(), development);
        model.with_model_io(self.sink(files, retention))
    }

    /// 写者（第一次调用时启动）的入队口。
    pub(crate) fn sink(&self, files: Files, retention: Retention) -> Arc<dyn ModelIoSink> {
        let writer = self.writer.get_or_init(|| Writer::start(files, retention));
        Arc::new(Sink {
            sender: writer.sender.clone(),
            dropped: writer.dropped.clone(),
        })
    }

    /// 等写者写完已经入队的记录；没有打开过时立即返回。
    pub(crate) async fn flush(&self) {
        let Some(writer) = self.writer.get() else {
            return;
        };
        let (reply, flushed) = oneshot::channel();
        if writer.sender.send(Item::Flush(reply)).await.is_ok() {
            let _ = flushed.await;
        }
    }
}

impl Writer {
    fn start(files: Files, retention: Retention) -> Self {
        let (sender, receiver) = mpsc::channel(CAPACITY);
        let dropped = Arc::new(AtomicU64::new(0));
        let scope = TaskScope::root("model-io", |_| {});
        let _writer = scope.spawner().spawn(
            "model-io-writer",
            write_loop(receiver, files, retention, dropped.clone()),
        );
        Self {
            _scope: scope,
            sender,
            dropped,
        }
    }
}

struct Sink {
    sender: mpsc::Sender<Item>,
    dropped: Arc<AtomicU64>,
}

impl ModelIoSink for Sink {
    fn record(&self, record: ModelIoRecord) {
        if self
            .sender
            .try_send(Item::Record(Pending::from(record)))
            .is_err()
        {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// 写者任务：一次一条；丢过记录时下一条带丢弃数并记一行 warn。所有发送方都释放后结束。
pub(crate) async fn write_loop(
    mut receiver: mpsc::Receiver<Item>,
    mut files: Files,
    retention: Retention,
    dropped: Arc<AtomicU64>,
) {
    while let Some(item) = receiver.recv().await {
        match item {
            Item::Record(pending) => {
                let missed = dropped.swap(0, Ordering::Relaxed);
                if missed > 0 {
                    tracing::warn!(
                        event = "model_io.dropped",
                        dropped = missed,
                        "model-io records were dropped: the queue was full"
                    );
                }
                let full = retention().await;
                files.write(pending, full, missed).await;
            }
            Item::Flush(reply) => {
                let _ = reply.send(());
            }
        }
    }
}

#[cfg(test)]
#[path = "model_io_tests.rs"]
mod tests;
