use super::checkpoint_blobs as blobs;
use crate::{contract::RewindTransaction, domain::file_checkpoint::FileCheckpoint};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, OwnedMutexGuard};
use tokio_util::sync::CancellationToken;
#[derive(Clone, Serialize, Deserialize)]
struct File {
    path: String,
    before: Option<String>,
    after: String,
    mode: Option<u32>,
    after_mode: Option<u32>,
    ids: Vec<String>,
    tools: Vec<String>,
}
#[derive(Serialize, Deserialize)]
struct Journal {
    session: String,
    token: String,
    files: Vec<File>,
}
pub(super) struct Transaction {
    root: PathBuf,
    journal: Journal,
    preview: Value,
    _guard: OwnedMutexGuard<()>,
}
async fn plan(root: &Path, changes: &[FileCheckpoint]) -> Result<(Value, Vec<File>)> {
    let mut groups: BTreeMap<&str, Vec<&FileCheckpoint>> = BTreeMap::new();
    for change in changes.iter().filter(|c| !c.restored) {
        groups.entry(&change.path).or_default().push(change);
    }
    let (mut safe, mut unsafe_files, mut files) = (Vec::new(), Vec::new(), Vec::new());
    for (path, group) in groups {
        let first = group[0];
        let last = group.last().unwrap();
        let mut tools = group.iter().map(|c| c.tool.clone()).collect::<Vec<_>>();
        tools.sort();
        tools.dedup();
        let mut entry = json!({"path":path,"operationCount":group.len(),"toolNames":tools});
        let result = async {
            ensure!(
                group
                    .windows(2)
                    .all(|w| w[1].before.as_ref() == Some(&w[0].after)),
                "unsupported_checkpoint"
            );
            let current = blobs::current(Path::new(path))
                .await
                .map_err(|_| anyhow::anyhow!("file_read_failed"))?;
            let hash = current.as_deref().map(blobs::hash);
            // 准备已提交但真正写入未发生，或用户已恢复相同原字节，都没有可撤销的改动。
            if hash == first.before {
                return Ok::<_, anyhow::Error>(false);
            }
            entry["expectedHash"] = last.after.clone().into();
            if let Some(hash) = &hash {
                entry["currentHash"] = hash.clone().into();
            }
            ensure!(hash.as_ref() == Some(&last.after), "external_modified");
            for key in first.before.iter().chain(std::iter::once(&last.after)) {
                let path = blobs::path(root, key)?;
                ensure!(tokio::fs::try_exists(&path).await?, "checkpoint_missing");
                blobs::load(root, key)
                    .await
                    .map_err(|_| anyhow::anyhow!("checkpoint_unreadable"))?;
            }
            Ok(true)
        }
        .await;
        match result {
            Ok(false) => {}
            Ok(true) => {
                entry.as_object_mut().unwrap().remove("expectedHash");
                entry.as_object_mut().unwrap().remove("currentHash");
                entry["action"] = if first.before.is_some() {
                    "restore"
                } else {
                    "delete"
                }
                .into();
                safe.push(entry);
                files.push(File {
                    path: path.into(),
                    before: first.before.clone(),
                    after: last.after.clone(),
                    mode: first.mode,
                    after_mode: blobs::mode(Path::new(path)).await?,
                    ids: group.iter().map(|c| c.id.clone()).collect(),
                    tools,
                });
            }
            Err(error) => {
                entry["reason"] = error.to_string().into();
                unsafe_files.push(entry);
            }
        }
    }
    Ok((
        json!({"canApply":unsafe_files.is_empty()&&!safe.is_empty(),"safeFiles":safe,"unsafeFiles":unsafe_files,"ignoredFiles":[]}),
        files,
    ))
}
pub(super) async fn preview(root: &Path, changes: &[FileCheckpoint]) -> Result<Value> {
    Ok(plan(root, changes).await?.0)
}
fn journal_path(root: &Path, session: &str) -> PathBuf {
    root.join("checkpoints/journals")
        .join(format!("{}.json", blobs::hash(session.as_bytes())))
}
pub(super) async fn pending(root: &Path) -> Result<Vec<String>> {
    let mut dir = match tokio::fs::read_dir(root.join("checkpoints/journals")).await {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e.into()),
    };
    let mut ids = vec![];
    while let Some(entry) = dir.next_entry().await? {
        if entry.path().extension().is_some_and(|x| x == "json") {
            let j = read_journal(&entry.path()).await?;
            ids.push(j.session);
        }
    }
    Ok(ids)
}
async fn read_journal(path: &Path) -> Result<Journal> {
    ensure!(
        tokio::fs::metadata(path).await?.len() <= 8 * 1024 * 1024,
        "Rewind journal exceeds limit"
    );
    Ok(serde_json::from_slice(&tokio::fs::read(path).await?)?)
}
pub(super) async fn recover(
    root: &Path,
    session: &str,
    committed: Option<&str>,
    gate: Arc<Mutex<()>>,
) -> Result<()> {
    let _guard = gate.lock_owned().await;
    let path = journal_path(root, session);
    if !tokio::fs::try_exists(&path).await? {
        return Ok(());
    }
    let journal = read_journal(&path).await?;
    ensure!(journal.session == session, "Rewind journal owner mismatch");
    if committed != Some(journal.token.as_str()) {
        rollback(root, &journal.files).await?;
    }
    tokio::fs::remove_file(&path).await?;
    blobs::sync_parent(&path).await
}
pub(super) async fn begin(
    root: &Path,
    session: &str,
    token: &str,
    changes: &[FileCheckpoint],
    gate: Arc<Mutex<()>>,
) -> Result<Box<dyn RewindTransaction>> {
    let guard = gate.lock_owned().await;
    let (preview, files) = plan(root, changes).await?;
    ensure!(
        preview["canApply"] == true,
        "File rewind conflicts: {preview}"
    );
    let path = journal_path(root, session);
    ensure!(
        !tokio::fs::try_exists(&path).await?,
        "Pending rewind requires recovery"
    );
    let journal = Journal {
        session: session.into(),
        token: token.into(),
        files,
    };
    let bytes = serde_json::to_vec(&journal)?;
    ensure!(
        bytes.len() <= 8 * 1024 * 1024,
        "Rewind journal exceeds recovery limit"
    );
    tokio::fs::create_dir_all(path.parent().unwrap()).await?;
    let tmp = path.with_extension("tmp");
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)
        .await?;
    file.write_all(&bytes).await?;
    file.sync_all().await?;
    drop(file);
    tokio::fs::rename(&tmp, &path).await?;
    blobs::sync_parent(&path).await?;
    for file in &journal.files {
        if let Err(error) = restore(root, file).await {
            // 故障补偿也检查版本；外部新改动不能被自动回滚覆盖，journal 留给下次恢复。
            rollback(root, &journal.files).await?;
            tokio::fs::remove_file(&path).await?;
            return Err(error);
        }
    }
    Ok(Box::new(Transaction {
        root: root.into(),
        journal,
        preview,
        _guard: guard,
    }))
}
async fn restore(root: &Path, file: &File) -> Result<()> {
    let path = Path::new(&file.path);
    let current = blobs::current(path).await?;
    ensure!(
        current.as_deref().map(blobs::hash).as_ref() == Some(&file.after),
        "File changed before rewind: {}",
        file.path
    );
    match &file.before {
        Some(key) => {
            let bytes = blobs::load(root, key).await?;
            super::tool_files::atomic_write(
                path,
                &bytes,
                current.as_deref(),
                &CancellationToken::new(),
            )
            .await?;
            blobs::set_mode(path, file.mode).await?;
        }
        None => {
            tokio::fs::remove_file(path).await?;
        }
    }
    blobs::sync_parent(path).await
}
async fn rollback(root: &Path, files: &[File]) -> Result<()> {
    for file in files.iter().rev() {
        let path = Path::new(&file.path);
        let current = blobs::current(path).await?;
        let hash = current.as_deref().map(blobs::hash);
        if hash.as_ref() == Some(&file.after) {
            continue;
        }
        ensure!(
            hash == file.before,
            "External modification blocks rewind recovery: {}",
            file.path
        );
        let bytes = blobs::load(root, &file.after).await?;
        super::tool_files::atomic_write(
            path,
            &bytes,
            current.as_deref(),
            &CancellationToken::new(),
        )
        .await?;
        blobs::set_mode(path, file.after_mode).await?;
        blobs::sync_parent(path).await?;
    }
    Ok(())
}
#[async_trait::async_trait]
impl RewindTransaction for Transaction {
    fn preview(&self) -> Value {
        self.preview.clone()
    }
    fn checkpoint_ids(&self) -> Vec<String> {
        self.journal
            .files
            .iter()
            .flat_map(|f| f.ids.clone())
            .collect()
    }
    async fn finish(self: Box<Self>, commit: bool) -> Result<()> {
        if !commit {
            rollback(&self.root, &self.journal.files).await?;
        }
        let path = journal_path(&self.root, &self.journal.session);
        tokio::fs::remove_file(&path).await?;
        blobs::sync_parent(&path).await
    }
}

#[cfg(test)]
#[path = "file_rewind_tests.rs"]
mod tests;
