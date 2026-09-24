//! Plugin manifests of marketplace entries (Node `marketplace.ts`
//! `readPluginManifestFromRoot`, `createManifestFromMarketplaceEntry`,
//! `resolveInstalledPluginVersion`, `ensureMarketplaceEntryManifest`).
use crate::manifest::DEFAULT_VERSION;
use crate::market::Entry;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Catalog-only fields dropped from a synthesized `plugin.json`.
const CATALOG_FIELDS: [&str; 14] = [
    "source",
    "category",
    "tags",
    "strict",
    "displayName",
    "displayName_i18n",
    "description_i18n",
    "icon",
    "privacyPolicy",
    "termsOfService",
    "heroImage",
    "examplePrompts",
    "examplePrompts_i18n",
    "requiresPaidPlan",
];

/// Node `createManifestFromMarketplaceEntry`.
pub fn synthetic(entry: &Entry) -> Value {
    let mut raw = entry.raw.clone();
    if let Some(fields) = raw.as_object_mut() {
        for key in CATALOG_FIELDS {
            fields.remove(key);
        }
        fields.insert("name".into(), entry.name.clone().into());
        fields.insert(
            "version".into(),
            entry.version.as_deref().unwrap_or(DEFAULT_VERSION).into(),
        );
    }
    raw
}

/// A manifest read from a plugin root and the file it came from (`None`
/// for a synthesized one).
pub struct RootManifest {
    pub manifest: Value,
    pub path: Option<PathBuf>,
}

/// Node `readPluginManifestFromRoot`: `Err` carries the thrown message.
pub async fn read(root: &Path, entry: &Entry) -> Result<Option<RootManifest>, String> {
    if let Some(path) = crate::manifest::find(root).await {
        let parsed = match crate::fsx::read_json(&path).await {
            Ok(Ok(value)) => value,
            Ok(Err(message)) => return Err(message),
            Err(error) => return Err(error.to_string()),
        };
        let Value::Object(mut manifest) = parsed else {
            return Err("Plugin manifest must be a JSON object".into());
        };
        let name = crate::js::trim(
            manifest
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )
        .to_owned();
        if !crate::manifest::valid_name(&name) {
            return Err(format!("Invalid plugin name: {name}"));
        }
        manifest.insert("name".into(), name.into());
        if !manifest.get("version").is_some_and(Value::is_string) {
            manifest.insert("version".into(), DEFAULT_VERSION.into());
        }
        return Ok(Some(RootManifest {
            manifest: Value::Object(manifest),
            path: Some(path),
        }));
    }
    if entry.strict == Some(false) {
        return Ok(Some(RootManifest {
            manifest: synthetic(entry),
            path: None,
        }));
    }
    Ok(None)
}

/// Node `resolveInstalledPluginVersion`: the root manifest's version, then
/// the entry's, then `0.0.0`.
pub async fn version(root: &Path, entry: &Entry) -> String {
    if let Some(path) = crate::manifest::find(root).await
        && let Some(version) = crate::fsx::read_json_lenient(&path)
            .await
            .and_then(|m| m["version"].as_str().map(str::to_owned))
            .filter(|v| !crate::js::trim(v).is_empty())
    {
        return version;
    }
    entry
        .version
        .clone()
        .unwrap_or_else(|| DEFAULT_VERSION.to_owned())
}

/// Node `ensureMarketplaceEntryManifest`: a non-strict entry without a
/// manifest gets `.claude-plugin/plugin.json` synthesized from the entry.
pub async fn ensure(entry: &Entry, target: &Path) -> anyhow::Result<()> {
    if crate::manifest::find(target).await.is_some() || entry.strict != Some(false) {
        return Ok(());
    }
    let path = target.join(".claude-plugin").join("plugin.json");
    crate::store::write_json(&path, &synthetic(entry)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(raw: Value) -> Entry {
        crate::market::parse_manifest(&json!({"name":"m","plugins":[raw]}))
            .unwrap()
            .plugins
            .remove(0)
    }

    #[tokio::test]
    async fn entry_manifests_follow_node() {
        let loose =
            entry(json!({"name":"p","strict":false,"icon":"i","skills":"s","source":"./p"}));
        assert_eq!(
            synthetic(&loose),
            json!({"name":"p","version":"0.0.0","skills":"s"})
        );
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert_eq!(version(root, &loose).await, "0.0.0");
        assert!(read(root, &loose).await.unwrap().unwrap().path.is_none());
        assert!(
            read(root, &entry(json!({"name":"p"})))
                .await
                .unwrap()
                .is_none()
        );
        ensure(&loose, root).await.unwrap();
        let written = root.join(".claude-plugin/plugin.json");
        assert_eq!(
            crate::fsx::read_json_lenient(&written).await.unwrap()["version"],
            "0.0.0"
        );
        tokio::fs::write(&written, r#"{"name":" p ","version":"2.0.0"}"#)
            .await
            .unwrap();
        assert_eq!(version(root, &loose).await, "2.0.0");
        let read_back = read(root, &loose).await.unwrap().unwrap();
        assert_eq!(read_back.manifest["name"], "p");
        tokio::fs::write(&written, "[]").await.unwrap();
        assert_eq!(
            read(root, &loose).await.err().unwrap(),
            "Plugin manifest must be a JSON object"
        );
    }
}
