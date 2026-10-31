// 复制自 apps/zcode-host/crates/base/src/store/lock.rs（07ab266d4b）的回收部分。
//! 失效锁的回收（Node `removeAbandonedLock`）：只回收 owner 已退出，或没有 PID 且超过 grace 的锁。

use std::collections::HashMap;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use serde_json::Value;

use super::{alive, now_ms};

/// 元数据里的时间比观察时刻晚这么多以上时视为非法（Node `MAX_LOCK_METADATA_CLOCK_SKEW_MS`）。
const MAX_METADATA_CLOCK_SKEW_MS: f64 = 5.0 * 60_000.0;

fn mtime_ms(metadata: &std::fs::Metadata) -> Option<f64> {
    metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs_f64() * rust_cli_base::units::MS_PER_SECOND_F64)
}

/// 有限、非负、不明显在未来的时间戳才有效。非法时由调用方回退到 mtime 或首次观察时刻。
fn parse_timestamp(value: Option<f64>, observed_at: f64) -> Option<f64> {
    value.filter(|value| {
        value.is_finite() && *value >= 0.0 && *value <= observed_at + MAX_METADATA_CLOCK_SKEW_MS
    })
}

struct Metadata {
    created_at: Option<f64>,
    pid: Option<u32>,
}

fn parse_metadata(raw: &str, observed_at: f64) -> Metadata {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return Metadata {
            created_at: None,
            pid: None,
        };
    };
    // 0、负数、小数或超出范围的 PID 没有 owner 语义（Node 的 isSafeInteger && > 0）。
    let pid = value
        .get("pid")
        .and_then(Value::as_u64)
        .filter(|pid| *pid > 0)
        .and_then(|pid| u32::try_from(pid).ok());
    Metadata {
        created_at: parse_timestamp(value.get("createdAt").and_then(Value::as_f64), observed_at),
        pid,
    }
}

/// 双重非法时间戳只能从当前锁实例首次被看见时起算 grace；实例被替换后重新计时（Node
/// `createLockInstanceObserver`）。
#[derive(Default)]
pub(super) struct InstanceObserver {
    observations: HashMap<PathBuf, (f64, String)>,
}

impl InstanceObserver {
    fn observe(&mut self, path: &Path, metadata: &std::fs::Metadata, observed_at: f64) -> f64 {
        let identity = format!(
            "{:?}:{:?}",
            directory_identity(metadata),
            mtime_ms(metadata)
        );
        if let Some((first, previous)) = self.observations.get(path)
            && *previous == identity
        {
            return *first;
        }
        self.observations
            .insert(path.to_owned(), (observed_at, identity));
        observed_at
    }
}

/// 目录实例的身份：Unix 上是设备号 + inode；Windows 没有 inode，用创建时间。
#[cfg(unix)]
pub(super) fn directory_identity(metadata: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (metadata.dev(), metadata.ino())
}

#[cfg(not(unix))]
pub(super) fn directory_identity(metadata: &std::fs::Metadata) -> (u64, u64) {
    let created = metadata
        .created()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos() as u64);
    (created, 0)
}

async fn owner_reclaimable(
    owner_file: &Path,
    ownerless_grace: Duration,
    observer: &mut InstanceObserver,
) -> io::Result<bool> {
    let observed_at = now_ms();
    let raw = tokio::fs::read_to_string(owner_file).await?;
    let metadata = parse_metadata(&raw, observed_at);
    let created_at = match metadata.created_at {
        Some(created_at) => created_at,
        None => {
            let stat = tokio::fs::metadata(owner_file).await?;
            parse_timestamp(mtime_ms(&stat), observed_at)
                .unwrap_or_else(|| observer.observe(owner_file, &stat, observed_at))
        }
    };
    let owner_exited = metadata.pid.is_some_and(|pid| !alive::is_alive(pid));
    let ownerless_stale = metadata.pid.is_none()
        && observed_at - created_at
            >= ownerless_grace.as_secs_f64() * rust_cli_base::units::MS_PER_SECOND_F64;
    Ok(owner_exited || ownerless_stale)
}

pub(super) fn is_owner_entry(name: &str) -> bool {
    name.starts_with("owner-") && name.ends_with(".json")
}

pub(super) async fn list_dir(path: &Path) -> io::Result<Vec<String>> {
    let mut entries = Vec::new();
    let mut reader = tokio::fs::read_dir(path).await?;
    while let Some(entry) = reader.next_entry().await? {
        entries.push(entry.file_name().to_string_lossy().into_owned());
    }
    Ok(entries)
}

pub(super) enum Removal {
    Removed,
    Kept,
    Failed(io::Error),
}

pub(super) async fn remove_abandoned(
    lock_dir: &Path,
    ownerless_grace: Duration,
    observer: &mut InstanceObserver,
) -> Removal {
    match try_remove(lock_dir, ownerless_grace, observer).await {
        Ok(true) => Removal::Removed,
        Ok(false) => Removal::Kept,
        Err(error)
            if error.kind() == ErrorKind::NotFound
                || error.kind() == ErrorKind::DirectoryNotEmpty =>
        {
            Removal::Kept
        }
        Err(error) => Removal::Failed(error),
    }
}

async fn try_remove(
    lock_dir: &Path,
    ownerless_grace: Duration,
    observer: &mut InstanceObserver,
) -> io::Result<bool> {
    let stat = tokio::fs::metadata(lock_dir).await?;
    if !stat.is_dir() {
        // 兼容升级前遗留的单文件锁：内容在判断期间变了就不删。
        let raw = tokio::fs::read_to_string(lock_dir).await?;
        if !owner_reclaimable(lock_dir, ownerless_grace, observer).await?
            || tokio::fs::read_to_string(lock_dir).await? != raw
        {
            return Ok(false);
        }
        tokio::fs::remove_file(lock_dir).await?;
        return Ok(true);
    }
    let entries = list_dir(lock_dir).await?;
    let owners: Vec<&String> = entries.iter().filter(|name| is_owner_entry(name)).collect();
    if let [owner] = owners.as_slice() {
        let owner_file = lock_dir.join(owner);
        if !owner_reclaimable(&owner_file, ownerless_grace, observer).await? {
            return Ok(false);
        }
        // 按唯一 owner 文件删除即所有权校验：锁被替换后新 owner 文件名不同，rmdir 会失败。
        let _ = tokio::fs::remove_file(&owner_file).await;
        tokio::fs::remove_dir(lock_dir).await?;
        return Ok(true);
    }
    let observed_at = now_ms();
    let directory_timestamp = parse_timestamp(mtime_ms(&stat), observed_at)
        .unwrap_or_else(|| observer.observe(lock_dir, &stat, observed_at));
    if observed_at - directory_timestamp
        < ownerless_grace.as_secs_f64() * rust_cli_base::units::MS_PER_SECOND_F64
    {
        return Ok(false);
    }
    for owner in &owners {
        if !owner_reclaimable(&lock_dir.join(owner), ownerless_grace, observer).await? {
            return Ok(false);
        }
    }
    // 空目录（mkdir 后崩溃）或多 owner 的损坏目录：只删观察到的条目，新增条目会让 rmdir 失败。
    for entry in &entries {
        let _ = tokio::fs::remove_file(lock_dir.join(entry)).await;
    }
    tokio::fs::remove_dir(lock_dir).await?;
    Ok(true)
}
