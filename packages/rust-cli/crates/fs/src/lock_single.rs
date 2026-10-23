//! 单文件锁（docs/specs/rust-cli/config.md §4.2）：Node 项目 hooks 信任记录用的锁
//! （`apps/zcode-cli/packages/adapters/src/storage/workspace-hook-trust-store.ts` 的 `acquireLock` /
//! `removeStaleLock` / `releaseLockIfOwned`），与 credentials 用的目录锁（[`super::lock`]）不是一种。
//!
//! 锁是同目录的 `<文件>.lock` 普通文件，`create_new` 独占创建，内容 `{"pid":…, "startTime":…, "token":…}\n`
//! （0600；`startTime` 是本进程的启动时间，取不到时不写，Node 把它当作旧格式）。拿不到时每 10 ms 重试，5 s 超时；
//! 锁文件 mtime 超过 30 s 且（内容读不出、pid 已退出，或 pid 还活着但它当前进程的启动时间与锁里的 `startTime`
//! 相差超过 2 s——pid 被复用）时回收；释放时只删除 token 仍是自己的锁。修复线 F2：以前不写也不比 `startTime`，
//! 持有者崩溃后 pid 被复用时锁永远不回收，Node 写的这种锁同样如此。

use std::future::Future;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;

use super::lock::alive;
// `<file>.lock`，与目录锁同一个命名（F55）。
use super::lock::lock_dir_of as lock_path;

/// 等待与回收的参数（默认值同 Node）。
#[derive(Debug, Clone)]
pub struct SingleLockOptions {
    pub retry: Duration,
    pub timeout: Duration,
    pub stale: Duration,
}

impl Default for SingleLockOptions {
    fn default() -> Self {
        Self {
            retry: Duration::from_millis(10),
            timeout: Duration::from_millis(5_000),
            stale: Duration::from_millis(30_000),
        }
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "锁 token 的随机种子用当前纳秒（不是业务时间）；fs 在 kernel-api 之下，拿不到注入的 Clock"
)]
fn token() -> String {
    use std::hash::Hasher;
    let mut hasher = rust_cli_base::random::hasher();
    hasher.write_u128(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos()),
    );
    format!("{:016x}{:08x}", hasher.finish(), std::process::id())
}

/// Node `LOCK_START_TIME_TOLERANCE_MS`：`ps` / `/proc` 的精度加调度延迟。
const START_TIME_TOLERANCE_MS: u64 = 2_000;

/// 锁文件里的持有者（Node `readLockOwner`）。
struct Owner {
    pid: u32,
    token: String,
    /// 持有者进程的启动时间（墙钟毫秒）；旧格式没有。
    start: Option<u64>,
}

/// 读锁文件里的 `{pid, startTime?, token}`；读不出时为 None（Node 视为无主）。
async fn owner(path: &Path) -> Option<Owner> {
    let text = tokio::fs::read_to_string(path).await.ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    Some(Owner {
        pid: u32::try_from(value.get("pid")?.as_u64()?).ok()?,
        token: value.get("token")?.as_str()?.to_owned(),
        start: value.get("startTime").and_then(Value::as_u64),
    })
}

async fn create(path: &Path, content: &str) -> io::Result<()> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path).await?;
    let written = async {
        tokio::io::AsyncWriteExt::write_all(&mut file, content.as_bytes()).await?;
        tokio::io::AsyncWriteExt::flush(&mut file).await
    }
    .await;
    drop(file);
    if written.is_err() {
        // 只删自己刚独占创建的文件（Node 相同）。
        let _ = tokio::fs::remove_file(path).await;
    }
    written
}

/// Node `removeStaleLock`：超龄且（读不出 owner，或 owner 进程已退出）时删除。
#[expect(
    clippy::disallowed_methods,
    reason = "fs 是锁文件时间的入口（README §4 时间类）：锁的年龄 = 现在减修改时间（SystemTime::elapsed），同 Node"
)]
async fn remove_stale(path: &Path, stale: Duration) -> io::Result<()> {
    let metadata = match tokio::fs::metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let age = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .unwrap_or_default();
    if age <= stale {
        return Ok(());
    }
    let reclaim = match owner(path).await {
        None => true,
        Some(owner) if owner.pid == std::process::id() => false,
        Some(owner) if !alive::is_alive(owner.pid) => true,
        // pid 还活着：启动时间对不上说明 pid 已被复用（取不到或旧格式时保守，不回收）。
        Some(owner) => match (owner.start, alive::start_time_ms(owner.pid).await) {
            (Some(recorded), Some(current)) => recorded.abs_diff(current) > START_TIME_TOLERANCE_MS,
            _ => false,
        },
    };
    if reclaim {
        match tokio::fs::remove_file(path).await {
            Err(error) if error.kind() != ErrorKind::NotFound => return Err(error),
            Ok(()) | Err(_) => {}
        }
    }
    Ok(())
}

/// 在 `file` 的单文件锁内执行 `operation`；无论成败都释放（只删 token 仍是自己的锁）。父目录由调用方
/// 准备好。
pub async fn with_single_file_lock<T, F>(
    file: &Path,
    options: &SingleLockOptions,
    operation: F,
) -> io::Result<T>
where
    F: Future<Output = io::Result<T>>,
{
    let path = lock_path(file);
    let token = token();
    let pid = std::process::id();
    // Node 的键序 `pid, startTime, token`；启动时间取不到时不写（Node 当作旧格式）。
    let content = match alive::start_time_ms(pid).await {
        Some(start) => format!("{{\"pid\":{pid},\"startTime\":{start},\"token\":\"{token}\"}}\n"),
        None => format!("{{\"pid\":{pid},\"token\":\"{token}\"}}\n"),
    };
    let started = Instant::now();
    loop {
        match create(&path, &content).await {
            Ok(()) => break,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        remove_stale(&path, options.stale).await?;
        if started.elapsed() >= options.timeout {
            return Err(io::Error::new(
                ErrorKind::TimedOut,
                format!(
                    "Timed out acquiring Workspace Hook Trust store lock: {}",
                    path.display()
                ),
            ));
        }
        tokio::time::sleep(options.retry).await;
    }
    let mut guard = Held {
        path,
        token,
        released: false,
    };
    let result = operation.await;
    guard.release().await;
    result
}

/// 持有中的锁。持锁的 future 被取消时在 Drop 里同步释放，否则本进程存活期间锁不会被回收。
struct Held {
    path: PathBuf,
    token: String,
    released: bool,
}

impl Held {
    async fn release(&mut self) {
        self.released = true;
        if owner(&self.path)
            .await
            .is_some_and(|owner| owner.token == self.token)
        {
            let _ = tokio::fs::remove_file(&self.path).await;
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let owned = std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .is_some_and(|value| value.get("token").and_then(Value::as_str) == Some(&self.token));
        if owned {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
#[path = "lock_single_tests.rs"]
mod tests;
