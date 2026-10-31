//! Node 的 pid 锁文件（docs/specs/rust-cli/platform.md §10，工作线 F52）：设备身份 `telemetry-state.json` 用的
//! `telemetry-state.lock`，照 Node CLI `withTelemetryStateLock` / `removeStaleTelemetryLockIfNeeded`
//! （`apps/zcode-cli/packages/adapters/src/device/cli-device-mid.ts`）。与 credentials 的目录锁（[`super::lock`]）、
//! hooks 信任记录的单文件锁（[`super::lock_single`]）都不是一种。
//!
//! 锁是调用方给的路径上的普通文件，`create_new` 独占创建（Node `open(lockFile, "wx")`），内容
//! `{"createdAt":<毫秒>,"pid":<pid>}`（没有换行）。已存在时：mtime 满 `stale` 就删掉、立刻重试；不到时读出持有者，
//! 读得出且 pid 不在（`kill(pid, 0)` 失败且不是 EPERM）就删掉、立刻重试，否则等 `retry` 再试；一共 `attempts` 次，
//! 用完报超时。释放时删除锁文件（Node 不核对持有者）。

use std::future::Future;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use rust_cli_base::units;

use super::lock::alive;

/// 缺省：锁被占着时多久重试一次。
const DEFAULT_RETRY: Duration = Duration::from_millis(10);
/// 缺省：一共试几次。
const DEFAULT_ATTEMPTS: u32 = 200;
/// 缺省：锁文件的 mtime 满这么久就当作没人持有（5 分钟）。
const DEFAULT_STALE: Duration = Duration::from_secs(5 * units::SECONDS_PER_MINUTE);

/// 等待与回收的参数（默认值同 Node：10 ms、200 次、5 分钟）。
#[derive(Debug, Clone)]
pub struct PidLockOptions {
    pub retry: Duration,
    pub attempts: u32,
    pub stale: Duration,
}

impl Default for PidLockOptions {
    fn default() -> Self {
        Self {
            retry: DEFAULT_RETRY,
            attempts: DEFAULT_ATTEMPTS,
            stale: DEFAULT_STALE,
        }
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "Node 锁文件的 createdAt 是墙钟毫秒（README §4 时间类：fs 的锁文件可以取）；fs 在 kernel-api 之下，拿不到注入的 Clock"
)]
fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis())
}

async fn create(path: &Path, content: &str) -> io::Result<()> {
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await?;
    let written = async {
        tokio::io::AsyncWriteExt::write_all(&mut file, content.as_bytes()).await?;
        tokio::io::AsyncWriteExt::flush(&mut file).await
    }
    .await;
    drop(file);
    if written.is_err() {
        // 只删自己刚独占创建的文件（Node 的 finally 相同）。
        let _ = tokio::fs::remove_file(path).await;
    }
    written
}

/// 锁文件里的持有者 pid（Node `readTelemetryLockOwner`：`pid` 与 `createdAt` 都要是数字）；读不出时为 None。
async fn owner(path: &Path) -> Option<f64> {
    let text = tokio::fs::read_to_string(path).await.ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    value.get("createdAt")?.as_f64()?;
    value.get("pid")?.as_f64()
}

/// Node `isProcessAlive`：不是正整数的 pid 算不在。
fn alive(pid: f64) -> bool {
    pid.fract() == 0.0 && pid > 0.0 && pid <= f64::from(u32::MAX) && alive::is_alive(pid as u32)
}

/// Node `removeStaleTelemetryLockIfNeeded`：删了（可以立刻重试）时为真；读不到锁的元数据时为假。
#[expect(
    clippy::disallowed_methods,
    reason = "fs 是锁文件时间的入口（README §4 时间类）：锁的年龄 = 现在减修改时间（SystemTime::elapsed），同 Node"
)]
async fn remove_stale(path: &Path, stale: Duration) -> bool {
    let Ok(metadata) = tokio::fs::metadata(path).await else {
        return false;
    };
    let age = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .unwrap_or_default();
    if age < stale {
        match owner(path).await {
            Some(pid) if !alive(pid) => {}
            // 读不出持有者，或持有者还活着：等。
            _ => return false,
        }
    }
    let _ = tokio::fs::remove_file(path).await;
    true
}

/// 持有中的锁：正常结束时异步删除；持锁的 future 被丢弃（请求取消）时在 Drop 里同步删除，否则本进程存活期间
/// 别的等待者（包括本进程之后的请求）要等到超时。
struct Held {
    path: PathBuf,
    released: bool,
}

impl Drop for Held {
    fn drop(&mut self) {
        if !self.released {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// 在 `lock` 这个锁文件内执行 `operation`；无论成败都释放。父目录不存在时创建（Node 先 `mkdir -p`）。
pub async fn with_pid_lock<T, F>(
    lock: &Path,
    options: &PidLockOptions,
    operation: F,
) -> io::Result<T>
where
    F: Future<Output = io::Result<T>>,
{
    if let Some(parent) = lock.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    for _ in 0..options.attempts {
        let content = format!(
            "{{\"createdAt\":{},\"pid\":{}}}",
            now_ms(),
            std::process::id()
        );
        match create(lock, &content).await {
            Ok(()) => {
                let mut held = Held {
                    path: lock.to_path_buf(),
                    released: false,
                };
                let result = operation.await;
                held.released = true;
                let _ = tokio::fs::remove_file(&held.path).await;
                return result;
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                if !remove_stale(lock, options.stale).await {
                    tokio::time::sleep(options.retry).await;
                }
            }
            Err(error) => return Err(error),
        }
    }
    // Node 的文字（`CLI telemetry state lock timeout`），带上路径。
    Err(io::Error::new(
        ErrorKind::TimedOut,
        format!("CLI telemetry state lock timeout: {}", lock.display()),
    ))
}

#[cfg(test)]
#[path = "lock_pid_tests.rs"]
mod tests;
