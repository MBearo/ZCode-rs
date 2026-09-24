use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::path::{Component, Path, PathBuf};
use tokio::io::AsyncReadExt;

pub(super) fn home() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default()
}
pub(super) fn resolve(base: &Path, path: &str) -> PathBuf {
    let raw = path
        .strip_prefix("~/")
        .map(|p| home().join(p))
        .unwrap_or_else(|| base.join(path));
    let mut result = PathBuf::new();
    for part in raw.components() {
        match part {
            Component::CurDir => (),
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other),
        }
    }
    result
}
pub(super) async fn json_file(path: &Path) -> Result<Value> {
    let file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(json!({})),
        Err(e) => return Err(e.into()),
    };
    let mut bytes = vec![];
    file.take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .await?;
    ensure!(
        bytes.len() <= 4 * 1024 * 1024,
        "Extension configuration exceeds size limit"
    );
    Ok(serde_json::from_slice(&bytes)?)
}
pub(super) async fn project_directories(cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![];
    for dir in cwd.ancestors() {
        dirs.push(dir.to_owned());
        if tokio::fs::metadata(dir.join(".git")).await.is_ok() {
            return dirs;
        }
    }
    vec![cwd.to_owned()]
}
pub(super) fn strings(value: &Value) -> Vec<&str> {
    if let Some(s) = value.as_str() {
        vec![s]
    } else {
        value
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default()
    }
}

/// Plugin discovery for the runtime (skills, MCP, agents; spec rust-m10-plugins §3.5).
pub(super) async fn plugins(
    cwd: &Path,
    config: &Value,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<zcode_cli_plugins::Outcome> {
    let storage = zcode_cli_plugins::records::storage_root(config, &home());
    let lookup = |name: &str| std::env::var(name).ok();
    zcode_cli_plugins::discover(&zcode_cli_plugins::Request {
        config,
        storage: &storage,
        cwd,
        env: &lookup,
        cancel,
    })
    .await
}

/// `path` is inside `root` and no component on the way is a symlink.
pub(super) async fn contained_file(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let mut current = root.to_owned();
    for part in std::iter::once(None).chain(relative.components().map(Some)) {
        if let Some(part) = part {
            current.push(part);
        }
        if !tokio::fs::symlink_metadata(&current)
            .await
            .is_ok_and(|m| !m.file_type().is_symlink())
        {
            return false;
        }
    }
    true
}
