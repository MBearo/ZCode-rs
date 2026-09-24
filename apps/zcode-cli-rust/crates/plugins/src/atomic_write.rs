//! Atomic file replacement in plugin storage (Node `writeFileAtomically`) and
//! the per-storage-root mutation lock (Node `withPluginStorageLock`).
use super::atomic::{OWNER_ID, recover, sidecars};
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

static LOCKS: LazyLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(Default::default);

/// Serializes plugin storage mutations of one storage root in this process.
pub async fn lock(storage: &Path) -> tokio::sync::OwnedMutexGuard<()> {
    let gate = LOCKS
        .lock()
        .expect("storage locks")
        .entry(storage.to_owned())
        .or_default()
        .clone();
    gate.lock_owned().await
}

async fn remove_quietly(path: &Path) {
    let _ = tokio::fs::remove_file(path).await;
}

/// Node `writeFileAtomically`: a same-directory stage renamed over the target;
/// when the platform refuses to replace it, a standalone transaction keeps a
/// recoverable backup (the sidecar format Node readers understand).
pub async fn write_file(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    tokio::fs::create_dir_all(parent).await?;
    recover(path).await?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let stage = parent.join(format!(".{name}.stage-{}", uuid::Uuid::new_v4()));
    let (backup, transaction) = sidecars(path);
    let result = async {
        tokio::fs::write(&stage, data).await?;
        match tokio::fs::rename(&stage, path).await {
            Ok(()) => return Ok(false),
            Err(error) if !crate::fsx::exists(path).await => return Err(error),
            Err(_) => {}
        }
        let record = json!({"version":2,"stageName":stage.file_name().map(|n| n.to_string_lossy().into_owned()),
            "transactionId":uuid::Uuid::new_v4().to_string(),"ownerId":OWNER_ID.as_str(),
            "ownerPid":std::process::id(),"hadTarget":true,"mode":"standalone"});
        // 标记文件排他创建：并发 writer 不能覆写仍存活事务的回滚快照。
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = options.open(&transaction).await?;
        tokio::io::AsyncWriteExt::write_all(&mut file, format!("{record}\n").as_bytes()).await?;
        drop(file);
        tokio::fs::rename(path, &backup).await?;
        if let Err(error) = tokio::fs::rename(&stage, path).await {
            if !crate::fsx::exists(path).await {
                tokio::fs::rename(&backup, path).await?;
            }
            return Err(error);
        }
        Ok(true)
    }
    .await;
    remove_quietly(&stage).await;
    match result {
        Ok(moved) => {
            if moved {
                remove_quietly(&backup).await;
            }
            remove_quietly(&transaction).await;
            Ok(())
        }
        Err(error) => {
            // 目标已移走且未能提交时保留边车，交给下一次读取按 Node 规则恢复。
            if crate::fsx::exists(path).await {
                remove_quietly(&transaction).await;
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn files_are_replaced_and_locks_serialize_per_root() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/installed.json");
        write_file(&path, b"one").await.unwrap();
        write_file(&path, b"two").await.unwrap();
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "two");
        let mut names = crate::fsx::read_dir_names(path.parent().unwrap())
            .await
            .unwrap();
        names.retain(|(n, _)| n.starts_with('.'));
        assert!(names.is_empty(), "no stage or sidecar left: {names:?}");
        let first = lock(dir.path()).await;
        let root = dir.path().to_owned();
        let waiting = tokio::spawn(async move { lock(&root).await });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());
        drop(first);
        waiting.await.unwrap();
    }
}
