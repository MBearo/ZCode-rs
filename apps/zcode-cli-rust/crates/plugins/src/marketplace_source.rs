//! Reading a marketplace from its source without persisting it (Node
//! `loadMarketplaceFromSource` with `persist: false`). Spec
//! rust-m10-4-plugin-sources §4.2.
use crate::market::{Manifest, normalize, parse_manifest};
use crate::ports::Ports;
use anyhow::{Result, anyhow, bail};
use serde_json::Value;
use std::path::{Path, PathBuf};

const CLAUDE_MARKETPLACE_FILE: &str = ".claude-plugin/marketplace.json";

pub struct Loaded {
    pub manifest: Manifest,
    /// The directory whose tree becomes the marketplace snapshot.
    pub source_root: Option<PathBuf>,
    /// A temporary clone removed after use.
    pub cleanup: Option<PathBuf>,
}

/// Node `findMarketplaceManifestPath`.
pub async fn find_manifest(root: &Path, explicit: Option<&str>) -> Option<PathBuf> {
    let candidates = explicit
        .into_iter()
        .chain([CLAUDE_MARKETPLACE_FILE, crate::store::MARKETPLACE_FILE]);
    for candidate in candidates {
        if let Some(path) = crate::fsx::resolve_inside(root, candidate)
            && crate::fsx::is_file(&path).await
        {
            return Some(path);
        }
    }
    None
}

/// Node `parseRequiredMarketplaceManifest`.
pub fn parse_required(value: &Value) -> Result<Manifest> {
    parse_manifest(value).ok_or_else(|| anyhow!("Marketplace manifest is invalid"))
}

/// `JSON.parse(await readFile(path, "utf8"))`.
pub async fn read_json(path: &Path) -> Result<Value> {
    let bytes = tokio::fs::read(path).await?;
    Ok(serde_json::from_str(&String::from_utf8_lossy(&bytes))?)
}

/// JavaScript `String(value)` of a manifest name.
fn js_string(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Object(_) => "[object Object]".into(),
        Value::Array(items) => items
            .iter()
            .map(|v| {
                if v.is_null() {
                    String::new()
                } else {
                    js_string(v)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        other => other.to_string(),
    }
}

/// Node `resolveRepositoryMarketplaceSource`: `(checkout root, directory to
/// remove)`. Sparse checkouts keep system Git; otherwise a public GitHub
/// archive is tried first.
async fn repository(
    url: &str,
    reference: Option<&str>,
    sparse: &[String],
    ports: &Ports<'_>,
) -> Result<(PathBuf, PathBuf)> {
    if sparse.is_empty() {
        match crate::github_archive::resolve(ports, url, reference, None).await {
            Ok(resolved) => {
                let cleanup = resolved.cleanup.unwrap_or_else(|| resolved.path.clone());
                return Ok((resolved.path, cleanup));
            }
            Err(error) if !crate::github_archive::should_fallback(&error) => {
                return Err(crate::github_archive::fetch_error(url, &error));
            }
            Err(_) => {}
        }
    }
    let dir =
        crate::git::clone_marketplace(ports.git, url, reference, sparse, ports.cancel).await?;
    Ok((dir.clone(), dir))
}

async fn from_checkout(
    (root, cleanup): (PathBuf, PathBuf),
    explicit: Option<&str>,
    label: String,
) -> Result<Loaded> {
    let result = async {
        let Some(file) = find_manifest(&root, explicit).await else {
            bail!("{label}");
        };
        parse_required(&read_json(&file).await?)
    }
    .await;
    match result {
        Ok(manifest) => Ok(Loaded {
            manifest,
            source_root: Some(root),
            cleanup: Some(cleanup),
        }),
        Err(error) => {
            let failure = crate::store::cleanup(Some(&cleanup)).await;
            Err(crate::failure::append_cleanup(error, failure))
        }
    }
}

pub async fn load(source: &Value, ports: &Ports<'_>) -> Result<Loaded> {
    crate::failure::check(ports.cancel)?;
    let text = |key: &str| source[key].as_str();
    let sparse: Vec<String> = source["sparsePaths"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    match text("source").unwrap_or_default() {
        "settings" => {
            let raw = source["marketplace"].clone();
            if raw.is_null() {
                // Node 缺陷：normalizeMarketplaceManifest(undefined) 访问 metadata 抛 TypeError。
                bail!("Cannot read properties of undefined (reading 'metadata')");
            }
            let name = raw
                .get("name")
                .map_or_else(|| "undefined".to_owned(), js_string);
            Ok(Loaded {
                manifest: normalize(raw, name),
                source_root: None,
                cleanup: None,
            })
        }
        "file" => {
            let path = PathBuf::from(text("path").unwrap_or_default());
            let manifest = parse_required(&read_json(&path).await?)?;
            Ok(Loaded {
                manifest,
                source_root: Some(path.parent().unwrap_or(Path::new("")).to_owned()),
                cleanup: None,
            })
        }
        "directory" => {
            let root = PathBuf::from(text("path").unwrap_or_default());
            let Some(file) = find_manifest(&root, None).await else {
                bail!(
                    "Marketplace manifest not found in directory: {}",
                    root.display()
                );
            };
            let manifest = parse_required(&read_json(&file).await?)?;
            Ok(Loaded {
                manifest,
                source_root: Some(root),
                cleanup: None,
            })
        }
        "url" => {
            let headers = crate::http::source_headers(source);
            let url = text("url").unwrap_or_default();
            let value =
                crate::http::marketplace_json(ports.http, url, &headers, ports.cancel).await?;
            Ok(Loaded {
                manifest: parse_required(&value)?,
                source_root: None,
                cleanup: None,
            })
        }
        "github" => {
            let repo = text("repo").unwrap_or_default();
            let url = format!("https://github.com/{repo}.git");
            let root = repository(&url, text("ref"), &sparse, ports).await?;
            let label = format!("Marketplace manifest not found in GitHub repo: {repo}");
            from_checkout(root, text("path"), label).await
        }
        "git" => {
            let url = text("url").unwrap_or_default();
            let root = repository(url, text("ref"), &sparse, ports).await?;
            let label = format!("Marketplace manifest not found in git repo: {url}");
            from_checkout(root, text("path"), label).await
        }
        kind @ ("npm" | "hostPattern" | "pathPattern") => {
            Err(crate::failure::unsupported_marketplace(kind))
        }
        // Node 缺陷：switch 没有 default 分支，调用方随后读取 undefined.manifest。
        _ => bail!("Cannot read properties of undefined (reading 'manifest')"),
    }
}
