//! Atomic directory activation (Node `activateDirectoryAtomically`): a staged
//! copy replaces the target through a recoverable backup, committed once the
//! authority file records the transaction id. Spec rust-m10-4-plugin-sources §3.
use crate::atomic::{ACTIVE, Live, OWNER_ID, recover, sidecars};
use crate::store;
use anyhow::{Result, anyhow};
use serde_json::json;
use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use tokio_util::sync::CancellationToken;

/// Targets with an activation in progress in this process (Node `activeAtomicReservations`).
static RESERVED: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Default::default);

fn key(target: &Path) -> std::io::Result<PathBuf> {
    Ok(crate::fsx::normalize(&std::path::absolute(target)?))
}

fn already_active(target: &Path) -> anyhow::Error {
    anyhow!(
        "Atomic directory activation is already active: {}",
        target.display()
    )
}

/// Releases the in-process registration of a target.
fn release(key: &Path) {
    ACTIVE.lock().expect("atomic registry").remove(key);
    RESERVED.lock().expect("atomic reservations").remove(key);
}

pub struct Activation {
    key: PathBuf,
    target: PathBuf,
    backup: PathBuf,
    marker: PathBuf,
    moved: bool,
    pub transaction_id: String,
}

impl Activation {
    /// Drops the backup and the marker once the authority records the transaction.
    pub async fn finalize(self) {
        if self.moved {
            store::cleanup(Some(&self.backup)).await;
        }
        store::cleanup(Some(&self.marker)).await;
    }

    /// Restores the previous target.
    pub async fn rollback(self) -> Result<()> {
        store::remove_all(&self.target).await?;
        if self.moved {
            tokio::fs::rename(&self.backup, &self.target).await?;
        }
        store::cleanup(Some(&self.marker)).await;
        Ok(())
    }
}

impl Drop for Activation {
    fn drop(&mut self) {
        // finalize/rollback 之外被丢弃时仍解除登记；磁盘标记留给读取方按存活 owner 规则处理。
        release(&self.key);
    }
}

/// Node `fs.cp(source, target, {recursive: true, force: true})`: links are
/// recreated with relative targets resolved against the link's directory.
pub async fn copy_tree(source: &Path, target: &Path) -> std::io::Result<()> {
    let metadata = tokio::fs::symlink_metadata(source).await?;
    if metadata.file_type().is_symlink() {
        let mut link = tokio::fs::read_link(source).await?;
        if link.is_relative() {
            link = crate::fsx::normalize(&source.parent().unwrap_or(Path::new("")).join(link));
        }
        return symlink(&link, target).await;
    }
    if metadata.is_file() {
        tokio::fs::copy(source, target).await?;
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(std::io::Error::other(format!(
            "Cannot copy special file: {}",
            source.display()
        )));
    }
    tokio::fs::create_dir_all(target).await?;
    for (name, _) in crate::fsx::read_dir_names(source).await? {
        Box::pin(copy_tree(&source.join(&name), &target.join(&name))).await?;
    }
    tokio::fs::set_permissions(target, metadata.permissions()).await
}

#[cfg(unix)]
async fn symlink(link: &Path, target: &Path) -> std::io::Result<()> {
    tokio::fs::symlink(link, target).await
}

#[cfg(windows)]
async fn symlink(link: &Path, target: &Path) -> std::io::Result<()> {
    // Node 在 Windows 上按目标类型选择目录或文件链接。
    if tokio::fs::metadata(link).await.is_ok_and(|m| m.is_dir()) {
        tokio::fs::symlink_dir(link, target).await
    } else {
        tokio::fs::symlink_file(link, target).await
    }
}

pub struct Input<'a> {
    pub target: &'a Path,
    pub source: Option<&'a Path>,
    /// The authority file whose records carry `cacheTransactionId`.
    pub authority: Option<&'a Path>,
    pub cancel: &'a CancellationToken,
}

async fn write_marker(marker: &Path, record: &serde_json::Value, target: &Path) -> Result<()> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    // 标记排他创建：第二个进程不能覆写仍存活事务的 owner 与回滚快照。
    let mut file = match options.open(marker).await {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(already_active(target));
        }
        other => other?,
    };
    tokio::io::AsyncWriteExt::write_all(&mut file, format!("{record}\n").as_bytes()).await?;
    Ok(())
}

/// Stages `source` (or an empty directory), runs `prepare` on the staged
/// path and swaps it in. Cancellation is honored until the commit point.
pub async fn activate<F, Fut>(input: Input<'_>, prepare: F) -> Result<Activation>
where
    F: FnOnce(PathBuf) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    crate::failure::check(input.cancel)?;
    let target = input.target;
    let parent = target.parent().unwrap_or(Path::new(""));
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    tokio::fs::create_dir_all(parent).await?;
    recover(target).await?;
    let container = store::unique_dir(parent, &format!(".{name}.stage-")).await?;
    let key = key(target)?;
    let authority = match input.authority {
        Some(path) => Some(crate::fsx::normalize(&std::path::absolute(path)?)),
        None => None,
    };
    let (backup, marker) = sidecars(target);
    let mut activation = Activation {
        key: key.clone(),
        target: target.to_owned(),
        backup,
        marker,
        moved: false,
        transaction_id: uuid::Uuid::new_v4().to_string(),
    };
    let mut marker_written = false;
    let result = async {
        {
            // 固定 backup/标记只支持单写者：同一目标的第二个事务直接失败。
            let active = ACTIVE.lock().expect("atomic registry").contains_key(&key);
            let mut reserved = RESERVED.lock().expect("atomic reservations");
            if active || !reserved.insert(key.clone()) {
                // 未取得保留时不能解除他人的登记。
                activation.key = PathBuf::new();
                return Err(already_active(target));
            }
        }
        let staged = container.join("content");
        match input.source {
            Some(source) => copy_tree(source, &staged).await?,
            None => tokio::fs::create_dir_all(&staged).await?,
        }
        prepare(staged.clone()).await?;
        crate::failure::check(input.cancel)?;
        // 暂存完整后才进入提交点；之后不再响应取消。
        let had_target = tokio::fs::metadata(target).await.is_ok();
        let coordinated = authority.is_some();
        ACTIVE.lock().expect("atomic registry").insert(
            key.clone(),
            Live {
                authority: authority.clone(),
                had_target,
                coordinated,
                transaction_id: activation.transaction_id.clone(),
            },
        );
        let mut record = json!({"hadTarget":had_target,"mode":if coordinated {"coordinated"} else {"standalone"},
            "ownerId":OWNER_ID.as_str(),"ownerPid":std::process::id(),
            "stageName":container.file_name().map(|n| n.to_string_lossy().into_owned()),
            "transactionId":activation.transaction_id,"version":2});
        if let Some(authority) = &authority {
            record["authorityPath"] = authority.to_string_lossy().into_owned().into();
        }
        write_marker(&activation.marker, &record, target).await?;
        marker_written = true;
        if had_target {
            tokio::fs::rename(target, &activation.backup).await?;
            activation.moved = true;
        }
        if let Err(error) = tokio::fs::rename(&staged, target).await {
            let failure = anyhow::Error::from(error);
            if activation.moved && !crate::fsx::exists(target).await {
                // 备份无法恢复时保留事务标记，交给读取方按 Node 规则恢复。
                if let Err(restore) = tokio::fs::rename(&activation.backup, target).await {
                    return Err(crate::failure::append_cleanup(
                        failure,
                        Some(restore.to_string()),
                    ));
                }
                activation.moved = false;
            }
            store::cleanup(Some(&activation.marker)).await;
            marker_written = false;
            return Err(failure);
        }
        Ok(())
    }
    .await;
    store::cleanup(Some(&container)).await;
    match result {
        Ok(()) => Ok(activation),
        Err(error) => {
            if marker_written && !activation.moved {
                store::cleanup(Some(&activation.marker)).await;
            }
            Err(error)
        }
    }
}

#[cfg(test)]
#[path = "activation_tests.rs"]
mod tests;
