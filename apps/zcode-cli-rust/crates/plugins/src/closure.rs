//! Dependency closure of a marketplace plugin (Node `resolveDependencyClosure`
//! and `resolveDependencyClosureFromManifest`): dependencies first, the root
//! last. Spec rust-m10-4-plugin-sources §8.
use crate::market::Manifest;
use anyhow::{Result, anyhow, bail};
use std::collections::HashMap;
use std::path::Path;

/// Node `parsePluginId`.
pub fn parse_id(id: &str) -> Result<(&str, &str)> {
    crate::records::split_id(id)
        .ok_or_else(|| anyhow!("Plugin id must use <name>@<marketplace>: {id}"))
}

/// Node `qualifyDependency`.
fn qualify(dependency: &str, marketplace: &str) -> String {
    if dependency.contains('@') {
        dependency.to_owned()
    } else {
        format!("{dependency}@{marketplace}")
    }
}

struct Walk<'a> {
    storage: &'a Path,
    root_marketplace: &'a str,
    allow_cross: &'a [String],
    /// Manifests read so far; the root's may be supplied unsaved.
    manifests: HashMap<String, Option<Manifest>>,
    visiting: Vec<String>,
    visited: Vec<String>,
    closure: Vec<String>,
}

impl Walk<'_> {
    async fn manifest(&mut self, marketplace: &str) -> Result<Option<&Manifest>> {
        if !self.manifests.contains_key(marketplace) {
            let loaded = crate::market::manifest(self.storage, marketplace).await?;
            self.manifests.insert(marketplace.to_owned(), loaded);
        }
        Ok(self.manifests[marketplace].as_ref())
    }

    async fn visit(&mut self, id: String, required_by: String) -> Result<()> {
        let (name, marketplace) = parse_id(&id)?;
        let (name, marketplace) = (name.to_owned(), marketplace.to_owned());
        if marketplace != self.root_marketplace && !self.allow_cross.contains(&marketplace) {
            bail!("Cross-marketplace dependency is blocked: {id} required by {required_by}");
        }
        if self.visiting.contains(&id) {
            let mut cycle = self.visiting.clone();
            cycle.push(id);
            bail!("Plugin dependency cycle: {}", cycle.join(" -> "));
        }
        if self.visited.contains(&id) {
            return Ok(());
        }
        let Some(manifest) = self.manifest(&marketplace).await? else {
            bail!("Marketplace not found for dependency: {marketplace}");
        };
        let Some(entry) = manifest.plugins.iter().find(|p| p.name == name) else {
            bail!("Dependency not found: {id} required by {required_by}");
        };
        let dependencies = entry.dependencies.clone().unwrap_or_default();
        self.visiting.push(id.clone());
        for dependency in dependencies {
            Box::pin(self.visit(qualify(&dependency, &marketplace), id.clone())).await?;
        }
        self.visiting.pop();
        self.visited.push(id.clone());
        self.closure.push(id);
        Ok(())
    }
}

/// The closure of `name@marketplace`; `root` supplies an unsaved manifest of
/// the root marketplace (validation of a marketplace source).
pub async fn resolve(
    storage: &Path,
    marketplace: &str,
    name: &str,
    allow_cross: &[String],
    root: Option<&Manifest>,
) -> Result<Vec<String>> {
    let mut manifests = HashMap::new();
    if let Some(root) = root {
        manifests.insert(marketplace.to_owned(), Some(root.clone()));
    }
    let mut walk = Walk {
        storage,
        root_marketplace: marketplace,
        allow_cross,
        manifests,
        visiting: vec![],
        visited: vec![],
        closure: vec![],
    };
    let id = format!("{name}@{marketplace}");
    walk.visit(id.clone(), id).await?;
    Ok(walk.closure)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest(name: &str, plugins: serde_json::Value) -> Manifest {
        crate::market::parse_manifest(&json!({"name":name,"plugins":plugins})).unwrap()
    }

    #[tokio::test]
    async fn closures_order_dependencies_first_and_reject_bad_graphs() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path();
        let other = crate::store::market_manifest(storage, "other");
        tokio::fs::create_dir_all(other.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            &other,
            json!({"name":"other","plugins":[{"name":"x"}]}).to_string(),
        )
        .await
        .unwrap();
        let root = manifest(
            "m",
            json!([{"name":"a","dependencies":["b@^1.0",{"name":"x","marketplace":"other"}]},
                   {"name":"b","dependencies":["c"]},{"name":"c"},
                   {"name":"loop","dependencies":["loop"]}]),
        );
        let resolve = |name: &'static str, allow: Vec<String>| {
            let root = root.clone();
            async move { resolve(storage, "m", name, &allow, Some(&root)).await }
        };
        assert_eq!(
            resolve("a", vec!["other".into()]).await.unwrap(),
            vec!["c@m", "b@m", "x@other", "a@m"]
        );
        assert_eq!(
            resolve("a", vec![]).await.unwrap_err().to_string(),
            "Cross-marketplace dependency is blocked: x@other required by a@m"
        );
        assert_eq!(
            resolve("loop", vec![]).await.unwrap_err().to_string(),
            "Plugin dependency cycle: loop@m -> loop@m"
        );
        assert_eq!(
            resolve("zzz", vec![]).await.unwrap_err().to_string(),
            "Dependency not found: zzz@m required by zzz@m"
        );
        assert_eq!(
            super::resolve(storage, "none", "a", &[], None)
                .await
                .unwrap_err()
                .to_string(),
            "Marketplace not found for dependency: none"
        );
    }
}
