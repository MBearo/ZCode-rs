//! The root directory of a marketplace entry's plugin (Node
//! `resolvePluginSourceRoot`). Spec rust-m10-4-plugin-sources §5.
use crate::market::{Entry, Manifest};
use crate::ports::Ports;
use anyhow::{Result, bail};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct Resolved {
    pub path: PathBuf,
    /// A temporary checkout removed after use.
    pub cleanup: Option<PathBuf>,
}

impl Resolved {
    fn local(path: PathBuf) -> Self {
        Self {
            path,
            cleanup: None,
        }
    }
}

pub struct Input<'a> {
    pub entry: &'a Entry,
    pub marketplace: &'a str,
    /// The marketplace's source tree (validation of an unsaved marketplace).
    pub source_root: Option<&'a Path>,
    pub manifest: Option<&'a Manifest>,
}

/// Node `readRequiredPluginSourceString`.
fn required<'v>(source: &'v Map<String, Value>, field: &str, label: &str) -> Result<&'v str> {
    match source.get(field).and_then(Value::as_str) {
        Some(value) if !crate::js::trim(value).is_empty() => Ok(value),
        _ => bail!("Plugin {label} source requires a non-empty {field}"),
    }
}

/// Node `normalizeGitUrl`: `owner/repo` becomes a GitHub URL.
fn git_url(value: &str) -> String {
    let shorthand = !value.contains(':')
        && value
            .split_once('/')
            .is_some_and(|(a, b)| !a.is_empty() && !b.is_empty() && !b.contains('/'));
    if shorthand {
        format!("https://github.com/{value}.git")
    } else {
        value.to_owned()
    }
}

/// Node `resolveMarketplacePluginBaseDir`.
async fn base_dir(market_dir: &Path, manifest: Option<&Manifest>) -> PathBuf {
    if let Some(root) = manifest.and_then(|m| m.plugin_root.as_deref())
        && let Some(resolved) = crate::fsx::resolve_inside(market_dir, root)
        && crate::fsx::is_dir(&resolved).await
    {
        return resolved;
    }
    market_dir.to_owned()
}

/// Node `resolveRepositoryPluginSource`: a public GitHub repository is
/// fetched as an archive first; only Git-only semantics or 401/403/404 fall
/// back to system Git (`resolveGitPluginSource`).
async fn repository(
    url: &str,
    path: Option<&str>,
    reference: Option<&str>,
    pin: Option<String>,
    ports: &Ports<'_>,
) -> Result<Resolved> {
    let archive_pin = pin.as_deref().or(reference);
    match crate::github_archive::resolve(ports, url, archive_pin, path).await {
        Ok(resolved) => return Ok(resolved),
        Err(error) if !crate::github_archive::should_fallback(&error) => {
            return Err(crate::github_archive::fetch_error(url, &error));
        }
        Err(_) => {}
    }
    let dir =
        crate::git::clone_plugin(ports.git, url, reference, pin.as_deref(), ports.cancel).await?;
    let Some(path) = path else {
        return Ok(Resolved {
            path: dir.clone(),
            cleanup: Some(dir),
        });
    };
    let checked = crate::failure::check(ports.cancel);
    let subdir = crate::fsx::resolve_inside(&dir, path);
    let exists = match &subdir {
        Some(subdir) => crate::fsx::is_dir(subdir).await,
        None => false,
    };
    if checked.is_err() || !exists {
        let error = checked.err().unwrap_or_else(|| {
            anyhow::anyhow!("Plugin source subdirectory does not exist: {path}")
        });
        let cleanup = crate::store::cleanup(Some(&dir)).await;
        return Err(crate::failure::append_cleanup(error, cleanup));
    }
    Ok(Resolved {
        path: subdir.expect("checked subdirectory"),
        cleanup: Some(dir),
    })
}

pub async fn resolve(input: &Input<'_>, ports: &Ports<'_>) -> Result<Resolved> {
    crate::failure::check(ports.cancel)?;
    let entry = input.entry;
    let id = format!("{}@{}", entry.name, input.marketplace);
    let market_dir = match input.source_root {
        Some(root) => root.to_owned(),
        None => crate::store::market_dir(ports.storage, input.marketplace),
    };
    let loaded;
    let manifest = match input.manifest {
        Some(manifest) => Some(manifest),
        None => {
            loaded = crate::market::manifest(ports.storage, input.marketplace).await?;
            loaded.as_ref()
        }
    };
    let base = base_dir(&market_dir, manifest).await;
    match &entry.source {
        Some(Value::String(kind)) if kind == "filesystem" || kind == "sea" => {
            // 内置插件的条目只是缓存指针，直接定位已落盘的缓存目录。
            if let Some(cache) = &entry.cache_path
                && crate::fsx::is_dir(Path::new(cache)).await
            {
                return Ok(Resolved::local(cache.into()));
            }
            let version = entry
                .version
                .as_deref()
                .unwrap_or(crate::manifest::DEFAULT_VERSION);
            let computed =
                crate::store::cache_dir(ports.storage, input.marketplace, &entry.name, version);
            if crate::fsx::is_dir(&computed).await {
                return Ok(Resolved::local(computed));
            }
            bail!("Bundled plugin cache directory missing: {id}")
        }
        Some(Value::String(source)) => {
            let relative = source.strip_prefix("./").unwrap_or(source);
            if let Some(local) = crate::fsx::resolve_inside(&base, relative)
                && crate::fsx::is_dir(&local).await
            {
                return Ok(Resolved::local(local));
            }
            let fallback =
                crate::fsx::resolve(&std::env::current_dir().unwrap_or_default(), source);
            if crate::fsx::is_dir(&fallback).await {
                return Ok(Resolved::local(fallback));
            }
            bail!("Unsupported or missing plugin source: {source}")
        }
        Some(Value::Object(source)) => object(source, &id, ports).await,
        _ => {
            let by_name = base.join(&entry.name);
            if crate::fsx::is_dir(&by_name).await {
                return Ok(Resolved::local(by_name));
            }
            bail!("Plugin source is not supported for {id}")
        }
    }
}

async fn object(source: &Map<String, Value>, id: &str, ports: &Ports<'_>) -> Result<Resolved> {
    let text = |key: &str| source.get(key).and_then(Value::as_str);
    let pin = crate::market::identity_pin(Some(&Value::Object(source.clone())));
    let kind = text("source").unwrap_or_default();
    match kind {
        "directory" => {
            let raw = required(source, "path", "directory path")?;
            let path = crate::fsx::resolve(&std::env::current_dir().unwrap_or_default(), raw);
            if crate::fsx::is_dir(&path).await {
                return Ok(Resolved::local(path));
            }
            bail!("Plugin source directory does not exist: {}", path.display())
        }
        "github" => {
            let repo = required(source, "repo", "GitHub repo")?;
            let url = format!("https://github.com/{repo}.git");
            repository(&url, text("path"), text("ref"), pin, ports).await
        }
        "git" => {
            let url = required(source, "url", "Git URL")?;
            repository(url, text("path"), text("ref"), pin, ports).await
        }
        "url" => {
            let url = required(source, "url", "URL")?;
            match text("type").unwrap_or_default() {
                "zip" => crate::zip_source::resolve_plugin(ports, source, url).await,
                "" | "git" => repository(url, text("path"), text("ref"), pin, ports).await,
                other => Err(crate::failure::unsupported_plugin(&format!("url:{other}"))),
            }
        }
        "git-subdir" => {
            let path = required(source, "path", "git-subdir path")?;
            let url = git_url(required(source, "url", "git-subdir URL")?);
            repository(&url, Some(path), text("ref"), pin, ports).await
        }
        "npm" | "pip" => Err(crate::failure::unsupported_plugin(kind)),
        // 显式对象来源配置错误时不回退到市场内同名目录，否则会安装错误来源。
        _ => bail!(
            "Plugin source is invalid or unsupported for {id}: {}",
            if kind.is_empty() {
                "missing kind"
            } else {
                kind
            }
        ),
    }
}
