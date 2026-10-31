// 复制自 apps/zcode-host/crates/base/src/store/lock.rs（07ab266d4b）。改动：随机 token 不用 getrandom（与
// 临时文件名同一做法）；错误是 io::Error（超时为 `ErrorKind::TimedOut`）；回收与存活判断拆到 `lock/` 下。
//! 跨进程的目录锁（docs/specs/rust-cli/platform.md §5.5；Node `packages/shared/src/node/atomicFileLock.ts`、
//! `privateFilePersistence.ts`）。
//!
//! 锁是 `<file>.lock/` 目录，里面唯一的 `owner-<token>.json` 记录 `{pid, createdAt, token}`。Node CLI、App
//! 与本工程用同一形状，同时运行时互斥。回收只针对 owner 已退出、或无 PID 且超过 grace 的锁；等到上限仍
//! 拿不到就报超时，绝不越过上限去删别人的锁。
//!
//! 进程内不排队（Host 相同）：同一进程的并发调用照样经目录锁互斥（轮询等待），只是不保证先来先得。

pub(crate) mod alive;
mod reclaim;

use std::hash::Hasher;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reclaim::{InstanceObserver, Removal, is_owner_entry, list_dir, remove_abandoned};

/// 与 Node `ZCODE_FILE_LOCK_TIMEOUT_ERROR_CODE` 对应，出现在超时错误的文本开头。
pub const FILE_LOCK_TIMEOUT_CODE: &str = rust_cli_base::names::env::ZCODE_FILE_LOCK_TIMEOUT;

/// 缺省：第 n 次重试前的等待（毫秒，用完后保持最后一个）。
const DEFAULT_RETRY_DELAYS_MS: [u64; 5] = [25, 50, 100, 200, 400];
/// 缺省：没有 PID 的锁超过这个时长才回收。
const DEFAULT_OWNERLESS_GRACE: Duration = Duration::from_millis(100);
/// 缺省：最长等待。
const DEFAULT_MAX_WAIT: Duration = Duration::from_millis(8_000);
/// owner 文件名里随机 token 的进制（数字与小写字母）。
const TOKEN_RADIX: u32 = 36;

/// 等待与回收的参数。默认值与 Node `privateFilePersistence.ts` 相同。
#[derive(Debug, Clone)]
pub struct LockOptions {
    /// 第 n 次重试前的等待（用完后保持最后一个）。
    pub retry_delays: Vec<Duration>,
    /// 没有 PID 的锁超过这个时长才回收（实际取 `min(grace, max_wait / 2)`）。
    pub ownerless_grace: Duration,
    /// 最长等待；到了就报超时。
    pub max_wait: Duration,
}

impl Default for LockOptions {
    fn default() -> Self {
        Self {
            retry_delays: DEFAULT_RETRY_DELAYS_MS
                .into_iter()
                .map(Duration::from_millis)
                .collect(),
            ownerless_grace: DEFAULT_OWNERLESS_GRACE,
            max_wait: DEFAULT_MAX_WAIT,
        }
    }
}

/// 持有中的锁。`release` 只删除本 writer 的 owner 文件，锁被接管时不会碰到别人的 token。
#[derive(Debug)]
pub struct FileLock {
    lock_dir: PathBuf,
    owner_file: PathBuf,
    released: bool,
}

impl FileLock {
    pub async fn release(mut self) {
        self.released = true;
        let _ = tokio::fs::remove_file(&self.owner_file).await;
        let _ = tokio::fs::remove_dir(&self.lock_dir).await;
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        if !self.released {
            // 持锁的 future 被取消：同步清理（正常路径走 release）。
            let _ = std::fs::remove_file(&self.owner_file);
            let _ = std::fs::remove_dir(&self.lock_dir);
        }
    }
}

/// 锁目录 owner 文件的 `createdAt` 与过期判断用的墙钟（Node `Date.now()`）。
#[expect(
    clippy::disallowed_methods,
    reason = "Node 目录锁文件格式里的时间（`createdAt`）；fs 在 kernel-api 之下，拿不到注入的 Clock"
)]
pub(crate) fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64() * rust_cli_base::units::MS_PER_SECOND_F64)
        .unwrap_or_default()
}

/// Node `Math.random().toString(36).slice(2)` 的替代：进程内计数器区分同一毫秒内的多次获取，
/// RandomState 的随机种子区分 pid 复用后的进程（与临时文件名同一做法）。
fn random_base36() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut hasher = rust_cli_base::random::hasher();
    hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    let mut value = hasher.finish();
    let mut out = String::new();
    loop {
        let digit = (value % u64::from(TOKEN_RADIX)) as u32;
        out.push(char::from_digit(digit, TOKEN_RADIX).unwrap_or('0'));
        value /= u64::from(TOKEN_RADIX);
        if value == 0 {
            break;
        }
    }
    out
}

fn timeout_error(lock_dir: &Path, waited: Duration) -> io::Error {
    io::Error::new(
        ErrorKind::TimedOut,
        format!(
            "{FILE_LOCK_TIMEOUT_CODE}: Timed out after {}ms waiting for the ZCode file lock: {}",
            waited.as_millis(),
            lock_dir.display()
        ),
    )
}

/// 到上限时：回收时遇到的权限错误优先报出（Node 相同），否则报超时。
fn give_up(lock_dir: &Path, elapsed: Duration, removal: Option<io::Error>) -> io::Error {
    match removal {
        Some(error) if error.kind() == ErrorKind::PermissionDenied => error,
        Some(_) | None => timeout_error(lock_dir, elapsed),
    }
}

/// 建出的锁目录在拿到锁之前被别的等待者回收了（它看到的是 mkdir 之后、owner 文件写完之前的空目录）：重来。
/// 修 bug（F17）：这时建 owner 文件在 Linux 上报 ENOENT，在 macOS 上报 EINVAL（实测：并发 rmdir 时
/// `open(O_CREAT | O_EXCL)` 返回 EINVAL）。以前只认 NotFound，macOS 上同一进程的并发取锁偶发
/// `Invalid argument (os error 22)`，凭据写入失败。
fn lost_created(created: bool, error: &io::Error) -> bool {
    created && matches!(error.kind(), ErrorKind::NotFound | ErrorKind::InvalidInput)
}

/// `<file>.lock`：目录锁与单文件锁（lock_single）同一个命名（F55：以前两边各写一份）。
pub(crate) fn lock_dir_of(file: &Path) -> PathBuf {
    let mut name = file.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

/// 取得 `file` 的锁（不建父目录；见 [`with_file_lock`]）。
pub async fn acquire_file_lock(file: &Path, options: &LockOptions) -> io::Result<FileLock> {
    let lock_dir = lock_dir_of(file);
    let token = format!(
        "{}-{}-{}",
        std::process::id(),
        now_ms() as u64,
        random_base36()
    );
    let owner_name = format!("owner-{token}.json");
    let owner_file = lock_dir.join(&owner_name);
    let payload = format!(
        "{{\"pid\":{},\"createdAt\":{},\"token\":\"{token}\"}}\n",
        std::process::id(),
        now_ms() as u64
    );
    let ownerless_grace = options.ownerless_grace.min(options.max_wait / 2);
    let started = Instant::now();
    let mut observer = InstanceObserver::default();
    let mut last_removal_error: Option<io::Error> = None;

    for attempt in 0usize.. {
        let mut created = false;
        let outcome = try_create(&lock_dir, &owner_file, &owner_name, &payload, &mut created).await;
        let error = match outcome {
            Ok(()) => {
                return Ok(FileLock {
                    lock_dir,
                    owner_file,
                    released: false,
                });
            }
            Err(error) => error,
        };
        if created {
            let _ = tokio::fs::remove_file(&owner_file).await;
            let _ = tokio::fs::remove_dir(&lock_dir).await;
        }
        let lost_created = lost_created(created, &error);
        if error.kind() != ErrorKind::AlreadyExists && !lost_created {
            return Err(error);
        }
        // 等待者自己等了多久不能证明锁失效；到上限直接超时，不再做一次回收（避免越过上限误删）。
        let elapsed = started.elapsed();
        if elapsed >= options.max_wait {
            return Err(give_up(&lock_dir, elapsed, last_removal_error));
        }
        match remove_abandoned(&lock_dir, ownerless_grace, &mut observer).await {
            Removal::Removed => continue,
            Removal::Kept => {}
            Removal::Failed(error) => last_removal_error = Some(error),
        }
        let remaining = options.max_wait.saturating_sub(elapsed);
        let Some(delay) = options
            .retry_delays
            .get(attempt.min(options.retry_delays.len().saturating_sub(1)))
        else {
            return Err(give_up(&lock_dir, elapsed, last_removal_error));
        };
        if remaining.is_zero() {
            return Err(give_up(&lock_dir, elapsed, last_removal_error));
        }
        tokio::time::sleep((*delay).min(remaining)).await;
    }
    Err(timeout_error(&lock_dir, started.elapsed()))
}

/// mkdir → 写 owner 文件 → 确认目录没被替换、里面只有自己一个 owner。
async fn try_create(
    lock_dir: &Path,
    owner_file: &Path,
    owner_name: &str,
    payload: &str,
    created: &mut bool,
) -> io::Result<()> {
    tokio::fs::create_dir(lock_dir).await?;
    *created = true;
    let created_stat = tokio::fs::metadata(lock_dir).await?;
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    let mut owner = options.open(owner_file).await?;
    tokio::io::AsyncWriteExt::write_all(&mut owner, payload.as_bytes()).await?;
    tokio::io::AsyncWriteExt::flush(&mut owner).await?;
    drop(owner);
    let current_stat = tokio::fs::metadata(lock_dir).await?;
    let owners: Vec<String> = list_dir(lock_dir)
        .await?
        .into_iter()
        .filter(|name| is_owner_entry(name))
        .collect();
    if reclaim::directory_identity(&created_stat) != reclaim::directory_identity(&current_stat)
        || owners.len() != 1
        || owners[0] != owner_name
    {
        return Err(io::Error::new(
            ErrorKind::AlreadyExists,
            "ZCode file lock ownership changed during acquire",
        ));
    }
    Ok(())
}

/// 建好父目录，在跨进程锁内执行 `operation`（Node `withFileLock`）。无论 `operation` 成败都释放锁。
pub async fn with_file_lock<T, E: From<io::Error>>(
    file: &Path,
    options: &LockOptions,
    operation: impl AsyncFnOnce() -> Result<T, E>,
) -> Result<T, E> {
    if let Some(parent) = file
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        tokio::fs::create_dir_all(parent).await?;
    }
    let lock = acquire_file_lock(file, options).await?;
    let result = operation().await;
    lock.release().await;
    result
}

#[cfg(test)]
#[path = "lock_tests.rs"]
mod tests;
