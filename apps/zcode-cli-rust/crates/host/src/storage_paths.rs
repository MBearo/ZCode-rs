//! The Node session database and artifact root, read from the storage
//! configuration without writing config.
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
pub struct StoragePaths {
    pub database: PathBuf,
    pub artifacts: PathBuf,
}
pub fn home() -> Result<PathBuf> {
    Ok(std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .context("Home directory unavailable")?
        .into())
}
fn expand(path: &str, cwd: &Path, home: &Path) -> PathBuf {
    if let Some(tail) = path.strip_prefix("~/") {
        home.join(tail)
    } else {
        cwd.join(path)
    }
}
/// Locate the session database the Node runtime would use for `cwd`.
/// Storage paths come from the same layered configuration as Node (`storage.dir`,
/// `storage.sessionDbPath`, including `ZCODE_*` overrides).
pub fn resolve(cwd: &Path, config: &crate::domain::config::ConfigSnapshot) -> Result<StoragePaths> {
    let home = home()?;
    let database = config
        .str("storage", "sessionDbPath")
        .unwrap_or("~/.zcode/cli/db/db.sqlite");
    let root = config.str("storage", "dir").unwrap_or("~/.zcode");
    Ok(StoragePaths {
        database: expand(database, cwd, &home),
        artifacts: expand(root, cwd, &home).join("cli/artifacts"),
    })
}
