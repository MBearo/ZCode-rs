//! Plugin storage layout, JSON writes and temporary directories (Node
//! `marketplace.ts` `getMarketplaceManifestPath`, `getPluginCacheDir`,
//! `getPluginDataDir`, `writeJsonFile`; `helpers.ts`
//! `cleanupPluginSourceBestEffort`).
use crate::fsx::sanitize_id;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const KNOWN_MARKETPLACES: &str = "known_marketplaces.json";
pub const INSTALLED_PLUGINS: &str = "installed_plugins.json";
pub const MARKETPLACE_FILE: &str = "marketplace.json";
const CLEANUP_RETRY_DELAYS_MS: [u64; 3] = [0, 25, 100];
const RANDOM_SUFFIX_LENGTH: usize = 6;

/// `marketplaces/<id>`: the directory holding a marketplace snapshot.
pub fn market_dir(storage: &Path, marketplace: &str) -> PathBuf {
    storage.join("marketplaces").join(sanitize_id(marketplace))
}

pub fn market_manifest(storage: &Path, marketplace: &str) -> PathBuf {
    market_dir(storage, marketplace).join(MARKETPLACE_FILE)
}

pub fn cache_dir(storage: &Path, marketplace: &str, name: &str, version: &str) -> PathBuf {
    storage
        .join("cache")
        .join(sanitize_id(marketplace))
        .join(sanitize_id(name))
        .join(sanitize_id(version))
}

pub fn data_dir(storage: &Path, plugin_id: &str) -> PathBuf {
    storage.join("data").join(sanitize_id(plugin_id))
}

/// ISO-8601 UTC with milliseconds (`new Date().toISOString()`).
pub fn now() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

pub fn pretty(value: &Value) -> String {
    format!(
        "{}\n",
        serde_json::to_string_pretty(value).expect("JSON serializes")
    )
}

/// Node `writeJsonFile`: pretty JSON replaced atomically.
pub async fn write_json(path: &Path, value: &Value) -> std::io::Result<()> {
    crate::atomic_write::write_file(path, pretty(value).as_bytes()).await
}

fn random_suffix() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..RANDOM_SUFFIX_LENGTH].to_owned()
}

/// Node `mkdtemp(join(parent, prefix))`: a new directory `prefix` + 6 random characters.
pub async fn unique_dir(parent: &Path, prefix: &str) -> std::io::Result<PathBuf> {
    tokio::fs::create_dir_all(parent).await?;
    loop {
        let path = parent.join(format!("{prefix}{}", random_suffix()));
        match tokio::fs::create_dir(&path).await {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            other => return other.map(|()| path),
        }
    }
}

/// A new directory under the system temporary directory.
pub async fn temp_dir(prefix: &str) -> std::io::Result<PathBuf> {
    unique_dir(&std::env::temp_dir(), prefix).await
}

/// `rm(path, {force: true, recursive: true})`.
pub async fn remove_all(path: &Path) -> std::io::Result<()> {
    match tokio::fs::symlink_metadata(path).await {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
        Ok(m) if m.is_dir() => match tokio::fs::remove_dir_all(path).await {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        },
        Ok(_) => match tokio::fs::remove_file(path).await {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        },
    }
}

/// Node `cleanupPluginSourceBestEffort`: retried removal whose failure is
/// reported, never thrown.
pub async fn cleanup(path: Option<&Path>) -> Option<String> {
    let path = path?;
    let mut failure = None;
    for delay in CLEANUP_RETRY_DELAYS_MS {
        if delay > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        }
        match remove_all(path).await {
            Ok(()) => return None,
            Err(error) => failure = Some(error.to_string()),
        }
    }
    failure
}
