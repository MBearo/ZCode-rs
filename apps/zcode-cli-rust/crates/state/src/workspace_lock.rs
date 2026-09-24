//! The per-workspace runtime owner lock (a file lock in the data dir).
use anyhow::{Context, Result};
use std::path::PathBuf;

/// Locks `workspace` for this process; the lock lasts as long as the file.
pub async fn lock_workspace(dir: PathBuf, workspace: String) -> Result<std::fs::File> {
    use sha2::{Digest, Sha256};
    tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&dir)?;
        let key = format!("{:x}", Sha256::digest(workspace.as_bytes()));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join(format!("workspace-{key}.lock")))?;
        // SQLite 的写锁不能阻止第二个 actor 先读取旧状态并执行恢复，必须先锁 owner。
        file.try_lock()
            .context("Workspace runtime is already owned or cannot be locked")?;
        Ok(file)
    })
    .await?
}
