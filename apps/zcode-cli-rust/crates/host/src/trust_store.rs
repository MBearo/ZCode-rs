//! Workspace hook trust store file shared with the Desktop (Node
//! `adapters/src/storage/workspace-hook-trust-store.ts`): lock file, atomic
//! writes, corrupt files moved aside.
use crate::contract::{TrustLoad, TrustStorePort};
use crate::domain::hooks::trust::{self, Record};
use crate::process_clock::{alive, boot_time_ms, process_start_ms};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

const LOCK_TIMEOUT: Duration = Duration::from_millis(5_000);
const STALE_LOCK_MS: u64 = 30_000;
const LOCK_RETRY: Duration = Duration::from_millis(10);
const RENAME_RETRIES_MS: [u64; 5] = [50, 100, 200, 400, 800];
const START_TIME_TOLERANCE_MS: i64 = 2_000;

/// Node `resolveWorkspaceHookTrustStorePath`: `storage.dir` of the user config
/// file only (`~/x`, absolute, or relative to home), default `~/.zcode`.
pub async fn trust_store_path(home: &Path, user_config: &Path) -> Result<PathBuf> {
    let config = match tokio::fs::read_to_string(user_config).await {
        Ok(text) => serde_json::from_str::<Value>(text.as_str()).with_context(|| {
            format!(
                "Unable to read trusted user config for Workspace Hook Trust store: {}",
                user_config.display()
            )
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Value::Null,
        Err(error) => return Err(error).context("Unable to read trusted user config"),
    };
    let configured = config["storage"]["dir"]
        .as_str()
        .map(str::trim)
        .unwrap_or("");
    let root = if configured.is_empty() {
        home.join(".zcode")
    } else if let Some(rest) = configured.strip_prefix("~/") {
        home.join(rest)
    } else {
        // 安全原因：相对 storage.dir 绑定用户目录，不能随 workspace cwd 漂移。
        home.join(configured)
    };
    Ok(root.join("security").join("workspace-hook-trust-v1.json"))
}

pub struct FileTrustStore {
    file: PathBuf,
    lock: PathBuf,
    /// In-process FIFO (tokio's mutex is fair) in front of the lock file.
    queue: tokio::sync::Mutex<()>,
}

impl FileTrustStore {
    pub fn new(file: PathBuf) -> Self {
        let mut lock = file.clone().into_os_string();
        lock.push(".lock");
        Self {
            file,
            lock: lock.into(),
            queue: tokio::sync::Mutex::new(()),
        }
    }

    async fn ensure_directory(&self) -> Result<()> {
        let directory = self.file.parent().context("Trust store has no directory")?;
        tokio::fs::create_dir_all(directory).await?;
        set_mode(directory, 0o700).await?;
        Ok(())
    }

    async fn locked<T>(&self, operation: impl AsyncFnOnce() -> Result<T>) -> Result<T> {
        let _queued = self.queue.lock().await;
        self.ensure_directory().await?;
        let mut guard = LockGuard {
            path: self.lock.clone(),
            token: Some(self.acquire().await?),
        };
        let result = operation().await;
        guard.release().await;
        result
    }

    async fn acquire(&self) -> Result<String> {
        let started = std::time::Instant::now();
        loop {
            let mut options = tokio::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            match options.open(&self.lock).await {
                Ok(mut handle) => {
                    let token = uuid::Uuid::new_v4().to_string();
                    let owner = format!(
                        "{{\"pid\":{},\"startTime\":{},\"token\":\"{token}\"}}\n",
                        std::process::id(),
                        boot_time_ms()
                    );
                    if let Err(error) = handle.write_all(owner.as_bytes()).await {
                        // 只可能删除自己用 create_new 独占创建的锁，不越权。
                        drop(handle);
                        let _ = tokio::fs::remove_file(&self.lock).await;
                        return Err(error.into());
                    }
                    return Ok(token);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    self.remove_stale().await?;
                    if started.elapsed() >= LOCK_TIMEOUT {
                        bail!(
                            "Timed out acquiring Workspace Hook Trust store lock: {}",
                            self.lock.display()
                        );
                    }
                    tokio::time::sleep(LOCK_RETRY).await;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    /// Node `removeStaleLock`: only locks older than 30 s whose owner is gone
    /// (dead pid, or a pid reused by another process instance).
    async fn remove_stale(&self) -> Result<()> {
        let metadata = match tokio::fs::metadata(&self.lock).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let age = metadata
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .map_or(0, |d| d.as_millis() as u64);
        if age <= STALE_LOCK_MS {
            return Ok(());
        }
        let Some(owner) = read_owner(&self.lock).await else {
            return remove(&self.lock).await;
        };
        if owner.pid == std::process::id() as i64 {
            return Ok(());
        }
        if !alive(owner.pid) {
            return remove(&self.lock).await;
        }
        let (Some(current), Some(recorded)) = (process_start_ms(owner.pid).await, owner.start)
        else {
            return Ok(());
        };
        if (current - recorded).abs() > START_TIME_TOLERANCE_MS {
            return remove(&self.lock).await;
        }
        Ok(())
    }

    /// Node `readCurrent(recoverCorrupt: true)`.
    async fn read_current(&self) -> Result<TrustLoad> {
        let content = match tokio::fs::read_to_string(&self.file).await {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(TrustLoad::Records(vec![]));
            }
            Err(error) => return Err(error.into()),
        };
        if let Some(records) = trust::parse_store(&content) {
            // 权限加固失败不影响读出的记录；下一次写入会再次尝试。
            let _ = set_mode(&self.file, 0o600).await;
            return Ok(TrustLoad::Records(records));
        }
        let mut aside = self.file.clone().into_os_string();
        aside.push(format!(".corrupt-{}", crate::now()));
        // 改名失败仍按损坏处理（fail-closed），原文件留在原地。
        if tokio::fs::rename(&self.file, &aside).await.is_ok() {
            let _ = set_mode(Path::new(&aside), 0o600).await;
        }
        tracing::warn!(
            target: "zcode::hooks",
            event = "workspace_hook.trust_store_corrupt",
            "Workspace Hook Trust store is corrupt and was moved aside"
        );
        Ok(TrustLoad::Corrupt)
    }

    async fn mutate(&self, update: impl FnOnce(Vec<Record>) -> Vec<Record>) -> Result<Vec<Record>> {
        self.locked(async || {
            let current = match self.read_current().await? {
                TrustLoad::Records(records) => records,
                TrustLoad::Corrupt => vec![],
            };
            let next = update(current);
            self.write(&next).await?;
            Ok(next)
        })
        .await
    }

    /// Node `atomicWrite`: temp file, fsync, rename (retried when the target
    /// is briefly busy), 0600.
    async fn write(&self, records: &[Record]) -> Result<()> {
        let directory = self.file.parent().context("Trust store has no directory")?;
        let name = self.file.file_name().unwrap_or_default().to_string_lossy();
        let temp = directory.join(format!(
            ".{name}.{}.{}.{:x}.tmp",
            std::process::id(),
            crate::now(),
            uuid::Uuid::new_v4().as_u128() as u64
        ));
        let written = async {
            let mut options = tokio::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            let mut handle = options.open(&temp).await?;
            handle
                .write_all(trust::store_content(records).as_bytes())
                .await?;
            handle.sync_all().await?;
            drop(handle);
            let mut attempt = 0;
            loop {
                match tokio::fs::rename(&temp, &self.file).await {
                    Ok(()) => break,
                    Err(error) if attempt < RENAME_RETRIES_MS.len() && retryable(&error) => {
                        tokio::time::sleep(Duration::from_millis(RENAME_RETRIES_MS[attempt])).await;
                        attempt += 1;
                    }
                    Err(error) => return Err(anyhow::Error::from(error)),
                }
            }
            set_mode(&self.file, 0o600).await
        }
        .await;
        if written.is_err() {
            let _ = tokio::fs::remove_file(&temp).await;
        }
        written
    }
}

#[async_trait]
impl TrustStorePort for FileTrustStore {
    async fn load(&self) -> Result<TrustLoad> {
        self.locked(async || self.read_current().await).await
    }
    async fn grant(&self, records: Vec<Record>) -> Result<Vec<Record>> {
        self.mutate(|mut current| {
            for record in records {
                match current.iter_mut().find(|r| r.key() == record.key()) {
                    Some(slot) => *slot = record,
                    None => current.push(record),
                }
            }
            current
        })
        .await
    }
    async fn revoke(&self, identity: &str, digests: Option<Vec<String>>) -> Result<Vec<Record>> {
        if digests.as_ref().is_some_and(Vec::is_empty) {
            // 空数组既不是“全部”也没有目标，任何 IO 之前拒绝。
            bail!("hookDeclarationDigests must be undefined or non-empty");
        }
        self.mutate(|current| {
            current
                .into_iter()
                .filter(|r| {
                    r.workspace_identity != identity
                        || digests
                            .as_ref()
                            .is_some_and(|d| !d.contains(&r.hook_declaration_digest))
                })
                .collect()
        })
        .await
    }
}

/// The lock file this process created. Released explicitly; a dropped
/// operation (cancelled future) still frees it so the process never waits on
/// its own lock.
struct LockGuard {
    path: PathBuf,
    token: Option<String>,
}

impl LockGuard {
    async fn release(&mut self) {
        if let Some(token) = self.token.take() {
            release(&self.path, &token).await;
        }
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if let Some(token) = self.token.take()
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            let path = self.path.clone();
            handle.spawn(async move { release(&path, &token).await });
        }
    }
}

/// 释放前核验所有权：锁若已被判过期并易主，删除会破坏新持有者的互斥。
async fn release(path: &Path, token: &str) {
    if read_owner(path).await.is_some_and(|o| o.token == token) {
        let _ = tokio::fs::remove_file(path).await;
    }
}

struct Owner {
    pid: i64,
    token: String,
    start: Option<i64>,
}

async fn read_owner(path: &Path) -> Option<Owner> {
    let value: Value = serde_json::from_str(&tokio::fs::read_to_string(path).await.ok()?).ok()?;
    Some(Owner {
        pid: value["pid"].as_f64()? as i64,
        token: value["token"].as_str()?.to_owned(),
        start: value["startTime"].as_f64().map(|n| n as i64),
    })
}

async fn remove(path: &Path) -> Result<()> {
    match tokio::fs::remove_file(path).await {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}

fn retryable(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::PermissionDenied
        || matches!(error.raw_os_error(), Some(code) if code == 16 || code == 32)
}

async fn set_mode(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).await?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

#[cfg(test)]
#[path = "trust_store_tests.rs"]
mod tests;
