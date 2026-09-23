//! An import owns its files until the durable marker transfers ownership to storage.
use anyhow::{Context, Result, ensure};
use rusqlite::{
    Connection,
    backup::{Backup, StepResult},
};
use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

pub(super) fn check(cancel: &CancellationToken) -> Result<()> {
    ensure!(!cancel.is_cancelled(), "TS import cancelled");
    Ok(())
}
pub(super) fn lock(dir: &Path, cancel: &CancellationToken) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join("ts-import.lock"))?;
    loop {
        check(cancel)?;
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => return Err(error.into()),
        }
    }
}

pub(super) struct Attempt {
    pub root: PathBuf,
    pub backup: PathBuf,
    retained: bool,
}
impl Attempt {
    pub fn new(dir: &Path) -> Result<Self> {
        let id = super::id();
        let root = dir.join(format!("ts-import-{id}"));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&root)?;
        let attempt = Self {
            root,
            backup: dir.join(format!("ts-backup-{id}.sqlite")),
            retained: false,
        };
        sync_dir(dir)?;
        Ok(attempt)
    }
    pub fn snapshot(&self) -> PathBuf {
        self.root.join("snapshot.sqlite")
    }
    pub fn publish(&self) -> Result<()> {
        std::fs::rename(self.snapshot(), &self.backup)?;
        sync_dir(&self.root)?;
        sync_dir(self.backup.parent().context("Missing import directory")?)
    }
    pub fn retain(&mut self) {
        self.retained = true;
    }
}
impl Drop for Attempt {
    fn drop(&mut self) {
        if !self.retained {
            // 只删除本次独占目录；不能回收其他已提交导入共享的旧附件目录。
            let _ = cleanup(&self.root, &self.backup);
        }
    }
}
fn cleanup(root: &Path, backup: &Path) -> Result<()> {
    match std::fs::remove_file(backup) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    std::fs::remove_dir_all(root)?;
    sync_dir(root.parent().context("Missing import directory")?)
}
pub(super) fn recover(conn: &Connection, dir: &Path, cancel: &CancellationToken) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        check(cancel)?;
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(id) = name.to_str().and_then(|s| s.strip_prefix("ts-import-")) else {
            continue;
        };
        if !uuid::Uuid::try_parse(id).is_ok_and(|u| u.to_string() == id) {
            continue;
        }
        let backup = dir.join(format!("ts-backup-{id}.sqlite"));
        let committed: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM rust_legacy_import WHERE backup=?1)",
            [backup.to_string_lossy().as_ref()],
            |r| r.get(0),
        )?;
        // DB 是文件归属的事实源；崩溃发生在 COMMIT 后时必须保留备份和附件。
        if !committed {
            cleanup(&entry.path(), &backup)?;
        }
    }
    Ok(())
}
pub(super) fn copy(source: &Connection, path: &Path, cancel: &CancellationToken) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    let mut dest = Connection::open(path)?;
    let backup = Backup::new(source, &mut dest)?;
    copy_pages(&backup, cancel, || {})?;
    drop(backup);
    // 源库可能处于 WAL 模式；发布前折叠为自包含备份，避免丢失 sidecar。
    dest.execute_batch("PRAGMA journal_mode=DELETE;")?;
    dest.close().map_err(|(_, e)| e)?;
    file.sync_all()?;
    sync_dir(path.parent().context("Missing backup directory")?)
}
fn copy_pages(
    backup: &Backup<'_, '_>,
    cancel: &CancellationToken,
    mut stepped: impl FnMut(),
) -> Result<()> {
    loop {
        check(cancel)?;
        let step = backup.step(128)?;
        stepped();
        check(cancel)?;
        match step {
            StepResult::Done => return Ok(()),
            StepResult::Busy | StepResult::Locked => std::thread::sleep(Duration::from_millis(10)),
            _ => {}
        }
    }
}
fn sync_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_between_pages_cleans_owned_files_and_preserves_source() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let source = Connection::open_in_memory()?;
        source.execute_batch(
            "CREATE TABLE data(bytes BLOB); INSERT INTO data VALUES(zeroblob(2097152));",
        )?;
        let cancel = CancellationToken::new();
        let attempt = Attempt::new(dir.path())?;
        let mut dest = Connection::open(attempt.snapshot())?;
        let backup = Backup::new(&source, &mut dest)?;
        let result = copy_pages(&backup, &cancel, || cancel.cancel());
        assert!(result.is_err());
        assert!(backup.progress().remaining > 0);
        drop(backup);
        drop(dest);
        let root = attempt.root.clone();
        drop(attempt);
        assert!(!root.exists());
        assert_eq!(
            source.query_row("SELECT length(bytes) FROM data", [], |r| r.get::<_, u64>(0))?,
            2097152
        );
        Ok(())
    }
    #[test]
    fn cancelled_import_lock_wait_is_bounded() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let cancel = CancellationToken::new();
        let _held = lock(dir.path(), &cancel)?;
        let other = cancel.clone();
        let waiter = std::thread::spawn({
            let path = dir.path().to_path_buf();
            move || lock(&path, &other)
        });
        cancel.cancel();
        assert!(waiter.join().unwrap().is_err());
        Ok(())
    }
}
