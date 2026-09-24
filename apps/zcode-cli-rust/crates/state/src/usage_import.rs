//! TS usage rows of the imported sessions (spec rust-m9-usage-logs §2.5).
use super::usage::TABLES;
use anyhow::Result;
use rusqlite::{Connection, OpenFlags, params, params_from_iter, types::Value};
use tokio_util::sync::CancellationToken;

fn columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
    Ok(conn
        .prepare("SELECT name FROM pragma_table_info(?1)")?
        .query_map([table], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Copies the rows of the workspace's TS sessions; rows already present stay.
pub(super) fn copy(
    dest: &Connection,
    snapshot: &Connection,
    workspace: &str,
    cancel: &CancellationToken,
) -> Result<()> {
    let sessions: Vec<String> = snapshot
        .prepare(
            "SELECT id FROM session WHERE COALESCE(NULLIF(TRIM(workspace_id),''),directory)=?1",
        )?
        .query_map([workspace], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for table in TABLES {
        let target = format!("rust_{table}");
        let wanted = columns(dest, &target)?;
        // 旧 TS 库可能没有用量表或缺列：只复制两边都有的列。
        let shared: Vec<String> = columns(snapshot, table)?
            .into_iter()
            .filter(|c| wanted.contains(c))
            .collect();
        if !shared.contains(&"session_id".to_owned()) {
            continue;
        }
        let list = shared.join(",");
        let slots = vec!["?"; shared.len()].join(",");
        let mut select =
            snapshot.prepare(&format!("SELECT {list} FROM {table} WHERE session_id=?1"))?;
        let mut insert = dest.prepare(&format!(
            "INSERT OR IGNORE INTO {target}({list}) VALUES({slots})"
        ))?;
        for session in &sessions {
            super::legacy_attempt::check(cancel)?;
            let mut rows = select.query([session])?;
            while let Some(row) = rows.next()? {
                let values = (0..shared.len())
                    .map(|i| row.get::<_, Value>(i))
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                insert.execute(params_from_iter(values))?;
            }
        }
    }
    Ok(())
}

/// Workspaces imported before usage import existed: copy once from the import backup.
pub(super) fn backfill(
    dest: &mut Connection,
    source: &str,
    workspace: &str,
    cancel: &CancellationToken,
) -> Result<()> {
    dest.execute_batch("CREATE TABLE IF NOT EXISTS rust_usage_import(source TEXT NOT NULL,workspace TEXT NOT NULL,PRIMARY KEY(source,workspace));")?;
    let done: bool = dest.query_row(
        "SELECT EXISTS(SELECT 1 FROM rust_usage_import WHERE source=?1 AND workspace=?2)",
        params![source, workspace],
        |r| r.get(0),
    )?;
    if done {
        return Ok(());
    }
    let backup: String = dest.query_row(
        "SELECT backup FROM rust_legacy_import WHERE source=?1 AND workspace=?2",
        params![source, workspace],
        |r| r.get(0),
    )?;
    let tx = dest.transaction()?;
    // 备份已被清理时没有可补的数据，直接记为完成，不阻止启动。
    if std::path::Path::new(&backup).exists() {
        let snapshot = Connection::open_with_flags(&backup, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        copy(&tx, &snapshot, workspace, cancel)?;
    }
    mark(&tx, source, workspace)?;
    tx.commit()?;
    Ok(())
}

pub(super) fn mark(conn: &Connection, source: &str, workspace: &str) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS rust_usage_import(source TEXT NOT NULL,workspace TEXT NOT NULL,PRIMARY KEY(source,workspace));")?;
    conn.execute(
        "INSERT OR IGNORE INTO rust_usage_import VALUES(?1,?2)",
        params![source, workspace],
    )?;
    Ok(())
}
