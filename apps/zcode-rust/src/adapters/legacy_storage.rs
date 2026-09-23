//! Read-only TS database import. Runs exclusively on the storage worker.
use super::legacy_attempt::{Attempt, check};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OpenFlags, params};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
pub(super) struct ImportRequest {
    pub source: std::path::PathBuf,
    pub workspace: String,
    pub cwd: String,
    pub dir: std::path::PathBuf,
    pub artifacts: std::path::PathBuf,
    pub cancel: CancellationToken,
}
pub(super) fn import(dest: &mut Connection, request: ImportRequest) -> Result<()> {
    let ImportRequest {
        source,
        workspace,
        cwd,
        dir,
        artifacts,
        cancel,
    } = request;
    let _lock = super::legacy_attempt::lock(&dir, &cancel)?;
    dest.execute_batch("CREATE TABLE IF NOT EXISTS rust_legacy_import(source TEXT NOT NULL,workspace TEXT NOT NULL,backup TEXT NOT NULL,PRIMARY KEY(source,workspace));")?;
    super::legacy_attempt::recover(dest, &dir, &cancel)?;
    let source = std::fs::canonicalize(source)?;
    let source_id = format!("{:x}", Sha256::digest(source.to_string_lossy().as_bytes()));
    let imported: bool = dest.query_row(
        "SELECT EXISTS(SELECT 1 FROM rust_legacy_import WHERE source=?1 AND workspace=?2)",
        params![source_id, workspace],
        |r| r.get(0),
    )?;
    if imported {
        return super::legacy_todos::backfill(dest, &source_id, &workspace, &cancel);
    }
    let conn = Connection::open_with_flags(&source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.busy_timeout(std::time::Duration::from_millis(20))?;
    let has_sequence: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('message') WHERE name='sequence')",
        [],
        |r| r.get(0),
    )?;
    ensure!(
        has_sequence,
        "TS database needs its existing schema migrations before import"
    );
    let mut attempt = Attempt::new(&dir)?;
    super::legacy_attempt::copy(&conn, &attempt.snapshot(), &cancel)?;
    drop(conn);
    let snapshot =
        Connection::open_with_flags(attempt.snapshot(), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    // 原先逐会话提交会在失败时留下半份历史，重启再次复制整个大库。标记和历史必须原子提交。
    let tx = dest.transaction()?;
    super::legacy_sessions::project(
        &tx,
        &snapshot,
        &workspace,
        &cwd,
        &attempt.root,
        &artifacts,
        &cancel,
    )?;
    drop(snapshot);
    check(&cancel)?;
    attempt.publish()?;
    tx.execute(
        "INSERT INTO rust_legacy_import VALUES(?1,?2,?3)",
        params![source_id, workspace, attempt.backup.to_string_lossy()],
    )?;
    check(&cancel)?;
    let committed = tx.commit();
    // COMMIT 的 IO 错误可能无法立即判定落盘结果；无法查询时保留文件，等下次持锁恢复。
    if committed.is_err() {
        let referenced = dest.query_row(
            "SELECT EXISTS(SELECT 1 FROM rust_legacy_import WHERE backup=?1)",
            [attempt.backup.to_string_lossy().as_ref()],
            |r| r.get::<_, bool>(0),
        );
        if referenced.unwrap_or(true) {
            attempt.retain();
        }
    }
    committed?;
    attempt.retain();
    Ok(())
}
