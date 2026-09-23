//! Read existing storage configuration without running TS migrations or writing config.
use anyhow::{Context, Result, ensure};
use serde_json::Value;
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
pub async fn resolve(
    explicit: Option<PathBuf>,
    cwd: &Path,
    automatic: bool,
) -> Result<Option<LegacySource>> {
    let env = std::env::var_os("ZCODE_SESSION_DB_PATH")
        .or_else(|| std::env::var_os("ZCODE_SESSION_DB"))
        .map(PathBuf::from);
    if explicit.is_none() && env.is_none() && !automatic {
        return Ok(None);
    }
    let home = home()?;
    let mut paths = vec![home.join(".zcode/cli/config.json")];
    let mut dirs = vec![];
    let mut found = false;
    for dir in cwd.ancestors() {
        dirs.push(dir);
        if tokio::fs::try_exists(dir.join(".git")).await? {
            found = true;
            break;
        }
    }
    if !found {
        dirs = vec![cwd];
    }
    for dir in dirs.into_iter().rev() {
        paths.extend([dir.join("zcode.json"), dir.join(".zcode/config.json")]);
    }
    let mut database = "~/.zcode/cli/db/db.sqlite".to_owned();
    let mut root = "~/.zcode".to_owned();
    for path in paths {
        let bytes = match tokio::fs::read(path).await {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        ensure!(
            bytes.len() <= 8 * 1024 * 1024,
            "Legacy config exceeds size limit"
        );
        let Ok(v) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        if let Some(value) = v["storage"]["sessionDbPath"]
            .as_str()
            .filter(|v| !v.trim().is_empty())
        {
            database = value.to_owned();
        }
        if let Some(value) = v["storage"]["dir"]
            .as_str()
            .filter(|v| !v.trim().is_empty())
        {
            root = value.to_owned();
        }
    }
    let required = explicit.is_some();
    if let Some(value) = explicit.or(env) {
        database = value.to_string_lossy().into_owned();
    }
    if let Ok(value) = std::env::var("ZCODE_STORAGE_DIR") {
        root = value;
    }
    Ok(Some(LegacySource {
        database: expand(&database, cwd, &home),
        artifacts: expand(&root, cwd, &home).join("cli/artifacts"),
        required,
    }))
}
