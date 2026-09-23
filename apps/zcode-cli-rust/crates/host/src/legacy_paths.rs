//! Read existing storage configuration without running TS migrations or writing config.
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
pub struct LegacySource {
    pub database: PathBuf,
    pub artifacts: PathBuf,
    pub required: bool,
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
/// Locate the TS session database the Node runtime would use for `cwd`.
/// Storage paths come from the same layered configuration as Node (`storage.dir`,
/// `storage.sessionDbPath`, including `ZCODE_*` overrides); an explicit path wins.
pub async fn resolve(
    explicit: Option<PathBuf>,
    cwd: &Path,
    automatic: bool,
    config: &crate::domain::config::ConfigSnapshot,
) -> Result<Option<LegacySource>> {
    let env =
        std::env::var_os("ZCODE_SESSION_DB_PATH").or_else(|| std::env::var_os("ZCODE_SESSION_DB"));
    if explicit.is_none() && env.is_none() && !automatic {
        return Ok(None);
    }
    let home = home()?;
    let required = explicit.is_some();
    let database = match explicit {
        Some(path) => path.to_string_lossy().into_owned(),
        None => config
            .str("storage", "sessionDbPath")
            .unwrap_or("~/.zcode/cli/db/db.sqlite")
            .to_owned(),
    };
    let root = config.str("storage", "dir").unwrap_or("~/.zcode");
    Ok(Some(LegacySource {
        database: expand(&database, cwd, &home),
        artifacts: expand(root, cwd, &home).join("cli/artifacts"),
        required,
    }))
}
