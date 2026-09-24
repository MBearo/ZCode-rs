//! Approved plan file `<workspace>/.zcode/plans/plan-<session>.md` (Node
//! `plan-file-continuity.ts`).
use crate::domain::plan_mode;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

fn path(cwd: &Path, session: &str) -> Result<PathBuf> {
    let name =
        plan_mode::plan_file_name(session).context("Session id cannot produce a plan file name")?;
    Ok(cwd.join(".zcode").join("plans").join(name))
}

pub(super) async fn write(cwd: &Path, session: &str, plan: &str) -> Result<()> {
    let path = path(cwd, session)?;
    let current = match tokio::fs::read(&path).await {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let cancel = CancellationToken::new();
    super::tool_files::atomic_write(&path, plan.as_bytes(), current.as_deref(), &cancel).await
}

/// `None` when the file is missing or blank; other read errors fail compaction.
pub(super) async fn read(cwd: &Path, session: &str) -> Result<Option<(String, String)>> {
    let path = path(cwd, session)?;
    let meta = match tokio::fs::metadata(&path).await {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if meta.len() > plan_mode::PLAN_FILE_MAX_BYTES {
        bail!("Plan file exceeds {} bytes", plan_mode::PLAN_FILE_MAX_BYTES);
    }
    let content = String::from_utf8_lossy(&tokio::fs::read(&path).await?).into_owned();
    if crate::domain::zod::js_trim(&content).is_empty() {
        return Ok(None);
    }
    Ok(Some((path.to_string_lossy().into_owned(), content)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn writes_atomically_and_reads_back_non_blank_plans() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read(dir.path(), "sess_1").await.unwrap().is_none());
        write(dir.path(), "sess_1", "step 1").await.unwrap();
        write(dir.path(), "sess_1", "step 2").await.unwrap();
        let (path, content) = read(dir.path(), "sess_1").await.unwrap().unwrap();
        assert!(path.ends_with(".zcode/plans/plan-sess_1.md"));
        assert_eq!(content, "step 2");
        write(dir.path(), "sess_1", "  ").await.unwrap();
        assert!(read(dir.path(), "sess_1").await.unwrap().is_none());
        assert!(write(dir.path(), "//", "x").await.is_err());
    }
}
