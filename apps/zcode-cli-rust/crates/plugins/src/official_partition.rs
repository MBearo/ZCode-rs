//! The official marketplace's partitions (Node `official-marketplace.ts`
//! `writeCdnOfficialMarketplacePartitionSync`): the refreshed CDN manifest
//! and the bundled seed merge into `marketplace.json`. Spec
//! rust-m10-4-plugin-sources §4.4.
use crate::official::MARKETPLACE;
use anyhow::{Result, bail};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

const BUNDLED: &str = "bundled-marketplace.json";
const CDN: &str = "cdn-marketplace.json";

fn partition(storage: &Path, file: &str) -> PathBuf {
    crate::store::market_dir(storage, MARKETPLACE).join(file)
}

/// Node `writeJsonFileSync`: identical content is not rewritten.
async fn write_if_changed(path: &Path, value: &Value) -> Result<()> {
    let contents = crate::store::pretty(value);
    if tokio::fs::read_to_string(path)
        .await
        .is_ok_and(|current| current == contents)
    {
        return Ok(());
    }
    tokio::fs::create_dir_all(path.parent().unwrap_or(Path::new(""))).await?;
    tokio::fs::write(path, contents).await?;
    Ok(())
}

async fn record(path: &Path) -> Option<Map<String, Value>> {
    match crate::fsx::read_json_lenient(path).await? {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

fn plugins(manifest: Option<&Map<String, Value>>) -> Vec<Value> {
    manifest
        .and_then(|m| m.get("plugins"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|p| p.is_object())
        .cloned()
        .collect()
}

fn name(plugin: &Value) -> Option<&str> {
    plugin["name"].as_str().filter(|n| !n.is_empty())
}

/// Writes the CDN partition and returns the rebuilt merged manifest.
pub async fn write_cdn(storage: &Path, manifest: &Value) -> Result<Value> {
    if manifest["name"] != MARKETPLACE {
        bail!("Official marketplace manifest must be named {MARKETPLACE}");
    }
    write_if_changed(&partition(storage, CDN), manifest).await?;
    let bundled = record(&partition(storage, BUNDLED))
        .await
        .filter(|p| p.get("version") == Some(&Value::from(1)))
        .and_then(|p| match p.get("manifest") {
            Some(Value::Object(m)) => Some(m.clone()),
            _ => None,
        });
    let cdn = record(&partition(storage, CDN)).await;
    let cdn_plugins = plugins(cdn.as_ref());
    let cdn_names: Vec<&str> = cdn_plugins.iter().filter_map(name).collect();
    // 同名插件以可刷新的 CDN 条目为准；只过滤合并目录，不删除内置缓存。
    let bundled_plugins = plugins(bundled.as_ref())
        .into_iter()
        .filter(|p| name(p).is_some_and(|n| !cdn_names.contains(&n)));
    let mut merged = bundled.unwrap_or_default();
    merged.extend(cdn.unwrap_or_default());
    merged.insert("name".into(), MARKETPLACE.into());
    merged.insert(
        "plugins".into(),
        Value::Array(cdn_plugins.iter().cloned().chain(bundled_plugins).collect()),
    );
    let merged = Value::Object(merged);
    write_if_changed(&partition(storage, crate::store::MARKETPLACE_FILE), &merged).await?;
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn cdn_entries_win_over_bundled_ones_of_the_same_name() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path();
        let bundled = json!({"version":1,"manifest":{"name":MARKETPLACE,"owner":"seed",
            "plugins":[{"name":"a","source":"filesystem"},{"name":"b","source":"filesystem"}]}});
        tokio::fs::create_dir_all(partition(storage, ""))
            .await
            .unwrap();
        tokio::fs::write(partition(storage, BUNDLED), bundled.to_string())
            .await
            .unwrap();
        let cdn = json!({"name":MARKETPLACE,"featured":["b"],"plugins":[{"name":"b","source":{"source":"git","url":"u"}}]});
        let merged = write_cdn(storage, &cdn).await.unwrap();
        assert_eq!(merged["owner"], "seed");
        assert_eq!(merged["featured"], json!(["b"]));
        let names: Vec<&str> = merged["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(name)
            .collect();
        assert_eq!(names, vec!["b", "a"]);
        assert_eq!(merged["plugins"][0]["source"]["source"], "git");
        let written = crate::fsx::read_json_lenient(&partition(storage, "marketplace.json"))
            .await
            .unwrap();
        assert_eq!(written, merged);
        assert!(write_cdn(storage, &json!({"name":"other"})).await.is_err());
    }
}
