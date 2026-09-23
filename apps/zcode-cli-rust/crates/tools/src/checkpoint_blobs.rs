use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;
pub(super) fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(super) fn path(root: &Path, hash: &str) -> Result<PathBuf> {
    ensure!(
        hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid checkpoint hash"
    );
    Ok(root.join("checkpoints/blobs").join(hash))
}
pub(super) async fn save(root: &Path, bytes: &[u8]) -> Result<String> {
    let key = hash(bytes);
    let path = path(root, &key)?;
    tokio::fs::create_dir_all(path.parent().unwrap()).await?;
    if tokio::fs::try_exists(&path).await? {
        load(root, &key).await?;
        return Ok(key);
    }
    let tmp = path.with_extension(super::id());
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .await?;
    file.write_all(bytes).await?;
    file.sync_all().await?;
    drop(file);
    tokio::fs::rename(&tmp, &path).await?;
    sync_parent(&path).await?;
    Ok(key)
}
pub(super) async fn load(root: &Path, key: &str) -> Result<Vec<u8>> {
    let path = path(root, key)?;
    ensure!(
        tokio::fs::metadata(&path).await?.len() <= 8 * 1024 * 1024,
        "Checkpoint exceeds limit"
    );
    let bytes = tokio::fs::read(path).await?;
    ensure!(hash(&bytes) == key, "Checkpoint content hash mismatch");
    Ok(bytes)
}
pub(super) async fn current(path: &Path) -> Result<Option<Vec<u8>>> {
    let meta = match tokio::fs::symlink_metadata(path).await {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    ensure!(
        meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= 8 * 1024 * 1024,
        "File is not a bounded regular file"
    );
    ensure!(
        tokio::fs::canonicalize(path).await? == path,
        "File parent changed or is a symlink"
    );
    Ok(Some(tokio::fs::read(path).await?))
}
pub(super) async fn sync_parent(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let parent = path.parent().context("Missing parent")?.to_owned();
        tokio::task::spawn_blocking(move || std::fs::File::open(parent)?.sync_all()).await??;
    }
    Ok(())
}
pub(super) async fn mode(path: &Path) -> Result<Option<u32>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match tokio::fs::metadata(path).await {
            Ok(m) => Ok(Some(m.permissions().mode())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(None)
    }
}
pub(super) async fn set_mode(path: &Path, mode: Option<u32>) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Some(mode) = mode {
            tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).await?;
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}
