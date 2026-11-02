//! Node 已安装插件与官方内置缓存的清单（docs/specs/rust-cli/ext.md §4.1、§4.2）。
use std::path::{Path, PathBuf};

use rust_cli_paths::StoragePaths;

use super::*;
use crate::test_support::write;

fn storage(dir: &Path) -> StoragePath {
    StoragePaths::resolve(&Default::default(), dir, dir).plugin_storage(None, dir)
}

#[tokio::test]
async fn installed_plugins_read_the_node_array_format() {
    let dir = tempfile::tempdir().unwrap();
    let root = storage(dir.path());
    assert!(installed_plugins(&root).await.is_empty());
    write(
        &root.as_path().join("installed_plugins.json"),
        r#"{"version": 1, "plugins": [
            {"id": "kit@market", "name": "kit", "marketplace": "market", "version": "1.2.0",
             "installPath": "/Users/dev/kit", "installedAt": "2026-10-01T00:00:00.000Z", "scope": "user"},
            {"id": "bad@market", "name": "bad", "marketplace": "market", "version": "1.0.0",
             "installPath": "/Users/dev/bad", "installedAt": "2026-10-01T00:00:00.000Z", "scope": "project"},
            {"id": "x y@my market", "name": "x y", "marketplace": "my market", "version": "0.1 beta",
             "installPath": "", "installedAt": "2026-10-01T00:00:00.000Z", "scope": "workspace"}
        ]}"#,
    )
    .await;
    let found = installed_plugins(&root).await;
    // `scope` 只能是 user / workspace（Node `isInstalledPluginRecord`），不合格的记录跳过。
    let ids: Vec<&str> = found.iter().map(|plugin| plugin.id.as_str()).collect();
    assert_eq!(ids, ["kit@market", "x y@my market"]);
    assert_eq!(found[0].marketplace, "market");
    assert_eq!(
        installed_root(&root, &found[0]),
        PathBuf::from("/Users/dev/kit")
    );
    // `installPath` 为空时用净化过的缓存目录（Node `getPluginCacheDir`）。
    assert_eq!(
        installed_root(&root, &found[1]),
        root.as_path().join("cache/my-market/x-y/0.1-beta")
    );
}

#[tokio::test]
async fn installed_plugins_read_the_map_format() {
    let dir = tempfile::tempdir().unwrap();
    let root = storage(dir.path());
    let file = root.as_path().join("installed_plugins.json");
    write(
        &file,
        r#"{"version": 2, "plugins": {
            "kit@market": [{"installPath": "/Users/dev/kit", "version": "1.0.0", "scope": "project"}],
            "solo@other": {"installPath": "/Users/dev/solo", "scope": "user"},
            "empty@market": [{"installPath": ""}],
            "noat": [{"installPath": "/Users/dev/noat"}],
            "trailing@": [{"installPath": "/Users/dev/trailing"}]
        }}"#,
    )
    .await;
    let found = installed_plugins(&root).await;
    let summary: Vec<(&str, &str, &str, &str)> = found
        .iter()
        .map(|plugin| {
            (
                plugin.id.as_str(),
                plugin.name.as_str(),
                plugin.marketplace.as_str(),
                plugin.version.as_str(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("kit@market", "kit", "market", "1.0.0"),
            ("solo@other", "solo", "other", "0.0.0"),
        ]
    );
    write(&file, "{not json").await;
    assert!(installed_plugins(&root).await.is_empty());
    write(&file, r#"{"plugins": 3}"#).await;
    assert!(installed_plugins(&root).await.is_empty());
}

#[tokio::test]
async fn official_roots_come_from_the_bundled_partition() {
    let dir = tempfile::tempdir().unwrap();
    let root = storage(dir.path());
    let cache = root.as_path().join("cache/zcode-plugins-official");
    let partition = root
        .as_path()
        .join("marketplaces/zcode-plugins-official/bundled-marketplace.json");
    let entry = |name: &str, path: &Path| serde_json::json!({"name": name, "cachePath": path.to_string_lossy(), "version": "1"});
    let manifest = serde_json::json!({"version": 1, "manifest": {
        "name": "zcode-plugins-official",
        "plugins": [
            entry("zcode-guide", &cache.join("zcode-guide/0.3.0")),
            // 越出该插件的缓存目录、指向插件目录本身、名字越出缓存目录：都不认。
            entry("evil", &cache.join("evil/../../outside")),
            entry("skill-creator", &cache.join("skill-creator")),
            entry("..", &cache.join("../x/1")),
            {"name": "no-path"},
            entry("skill-creator", &cache.join("skill-creator/0.1.0")),
        ]
    }});
    write(&partition, manifest.to_string()).await;
    assert_eq!(
        official_plugin_roots(&root).await,
        [
            cache.join("zcode-guide/0.3.0"),
            cache.join("skill-creator/0.1.0")
        ]
    );
}

#[tokio::test]
async fn official_roots_scan_the_cache_without_a_partition() {
    let dir = tempfile::tempdir().unwrap();
    let root = storage(dir.path());
    assert!(official_plugin_roots(&root).await.is_empty());
    let cache = root.as_path().join("cache/zcode-plugins-official");
    write(
        &cache.join("zcode-guide/0.3.0/.zcode-plugin/plugin.json"),
        "{}",
    )
    .await;
    write(
        &cache.join("zcode-guide/0.2.0/.zcode-plugin/plugin.json"),
        "{}",
    )
    .await;
    write(
        &cache.join("browser-use/0.5.1/.zcode-plugin/plugin.json"),
        "{}",
    )
    .await;
    write(&cache.join("stray.json"), "{}").await;
    // 版本不是 1 的分片按没有处理（Node `readBundledPartition`）。
    write(
        &root
            .as_path()
            .join("marketplaces/zcode-plugins-official/bundled-marketplace.json"),
        r#"{"version": 2, "manifest": {"plugins": []}}"#,
    )
    .await;
    assert_eq!(
        official_plugin_roots(&root).await,
        [
            cache.join("browser-use/0.5.1"),
            cache.join("zcode-guide/0.2.0"),
            cache.join("zcode-guide/0.3.0"),
        ]
    );
}
