//! Installing a marketplace plugin with its dependency closure (Node
//! `installMarketplacePlugin`, `cacheMarketplacePlugin`). Spec
//! rust-m10-4-plugin-sources §8.
use crate::activation::{self, Activation};
use crate::market::Entry;
use crate::plugin_source;
use crate::ports::Ports;
use crate::records::Installed;
use anyhow::{Result, anyhow, bail};
use serde_json::json;

pub struct Outcome {
    pub closure: Vec<String>,
    pub records: Vec<Installed>,
}

/// Node `assertZipPluginInstallRoot`.
async fn assert_zip_root(root: &std::path::Path, entry: &Entry, marketplace: &str) -> Result<()> {
    let loaded = crate::entry_manifest::read(root, entry)
        .await
        .map_err(|message| anyhow!(message))?;
    let Some(loaded) = loaded else {
        bail!("Plugin manifest not found: {}@{marketplace}", entry.name);
    };
    let name = loaded.manifest["name"].as_str().unwrap_or_default();
    if name != entry.name {
        bail!(
            "Plugin manifest name '{name}' does not match marketplace entry '{}'",
            entry.name
        );
    }
    Ok(())
}

/// Node `cacheMarketplacePlugin`: the resolved root is staged into
/// `cache/<market>/<name>/<version>`, committed with `installed_plugins.json`.
async fn cache(
    ports: &Ports<'_>,
    entry: &Entry,
    marketplace: &str,
    state: &mut Vec<Installed>,
) -> Result<(Installed, Option<Activation>)> {
    crate::failure::check(ports.cancel)?;
    let input = plugin_source::Input {
        entry,
        marketplace,
        source_root: None,
        manifest: None,
    };
    let resolved = plugin_source::resolve(&input, ports).await?;
    let staged = async {
        // 多顶层 ZIP 未显式 path 时会回退到解压根；删除旧缓存前必须确认它能形成合法插件。
        if crate::zip_source::is_zip(entry.source.as_ref()) {
            assert_zip_root(&resolved.path, entry, marketplace).await?;
        }
        // 缓存目录的版本段取插件自带 manifest 的真实版本，与安装记录、UI 展示一致。
        let version = crate::entry_manifest::version(&resolved.path, entry).await;
        let target = crate::store::cache_dir(ports.storage, marketplace, &entry.name, &version);
        let same = crate::fsx::normalize(&std::path::absolute(&resolved.path)?)
            == crate::fsx::normalize(&std::path::absolute(&target)?);
        let mut activation = None;
        if same {
            // 内置 filesystem/sea 插件的来源就是缓存目录本身，不能先删再自我拷贝。
            crate::entry_manifest::ensure(entry, &target).await?;
        } else {
            crate::failure::check(ports.cancel)?;
            let authority = ports.storage.join(crate::store::INSTALLED_PLUGINS);
            let input = activation::Input {
                target: &target,
                source: Some(&resolved.path),
                authority: Some(&authority),
                cancel: ports.cancel,
            };
            activation = Some(
                activation::activate(input, |staged| async move {
                    crate::entry_manifest::ensure(entry, &staged).await
                })
                .await?,
            );
        }
        anyhow::Ok((version, target, activation))
    }
    .await;
    // 缓存已复制成功后，临时目录清理失败不能阻断安装记录落盘。
    crate::store::cleanup(resolved.cleanup.as_deref()).await;
    let (version, target, activation) = staged?;
    let now = crate::store::now();
    let id = format!("{}@{marketplace}", entry.name);
    let mut raw = json!({"id":id,"name":entry.name,"marketplace":marketplace,"version":version,
        "installPath":target.to_string_lossy(),"installedAt":now,"updatedAt":now,"scope":"user"});
    if let Some(dependencies) = &entry.dependencies {
        raw["dependencies"] = json!(dependencies);
    }
    if let Some(source) = &entry.source {
        raw["source"] = source.clone();
    }
    if let Some(activation) = &activation {
        raw["cacheTransactionId"] = activation.transaction_id.clone().into();
    }
    let record = crate::records::from_record(&raw).expect("complete record");
    if let Some(existing) = state.iter_mut().find(|r| r.id == id) {
        let mut merged = existing.raw.clone();
        if let Some(fields) = merged.as_object_mut() {
            fields.remove("cacheTransactionId");
            let installed_at = fields.get("installedAt").filter(|v| !v.is_null()).cloned();
            for (key, value) in raw.as_object().into_iter().flatten() {
                fields.insert(key.clone(), value.clone());
            }
            if let Some(installed_at) = installed_at {
                fields.insert("installedAt".into(), installed_at);
            }
        }
        *existing = crate::records::from_record(&merged)
            .ok_or_else(|| anyhow!("Invalid installed plugin record: {id}"))?;
    } else {
        state.push(record.clone());
    }
    // 返回本次写入的记录（installedAt 为当前时间）；合并后的旧 installedAt 只进入持久化状态。
    Ok((record, activation))
}

/// Node `installMarketplacePlugin`: every plugin of the closure is cached,
/// then `installed_plugins.json` is written; any failure rolls back all
/// activations in reverse order.
pub async fn install(ports: &Ports<'_>, marketplace: &str, name: &str) -> Result<Outcome> {
    crate::marketplace_ops::ensure_available(ports, marketplace).await?;
    crate::failure::check(ports.cancel)?;
    let root = crate::market::manifest(ports.storage, marketplace).await?;
    let allow_cross = root.map(|m| m.allow_cross).unwrap_or_default();
    let closure =
        crate::closure::resolve(ports.storage, marketplace, name, &allow_cross, None).await?;
    let mut state = crate::records::installed(ports.storage).await?;
    let mut installed = vec![];
    let mut activations: Vec<Activation> = vec![];
    let result = async {
        for id in &closure {
            let (name, marketplace) = crate::closure::parse_id(id)?;
            let Some(manifest) = crate::market::manifest(ports.storage, marketplace).await? else {
                bail!("Marketplace not found: {marketplace}");
            };
            let Some(entry) = manifest.plugins.iter().find(|p| p.name == name) else {
                bail!("Plugin not found: {id}");
            };
            let (record, activation) = cache(ports, entry, marketplace, &mut state).await?;
            installed.push(record);
            activations.extend(activation);
        }
        crate::failure::check(ports.cancel)?;
        crate::records::save_installed(ports.storage, &state).await?;
        Ok(())
    }
    .await;
    if let Err(error) = result {
        let mut rollback = None;
        while let Some(activation) = activations.pop() {
            if let Err(e) = activation.rollback().await {
                rollback.get_or_insert(e.to_string());
            }
        }
        return Err(crate::failure::append_cleanup(error, rollback));
    }
    for activation in activations {
        activation.finalize().await;
    }
    Ok(Outcome {
        closure,
        records: installed,
    })
}
