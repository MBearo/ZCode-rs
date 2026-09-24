//! Materializing marketplaces (Node `addMarketplace`, `updateMarketplace`,
//! `ensureMarketplaceManifestAvailable`). Spec rust-m10-4-plugin-sources §7.
use crate::activation::{self, Activation};
use crate::marketplace_source::{self, Loaded};
use crate::official::MARKETPLACE;
use crate::ports::Ports;
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::path::Path;

pub struct Add<'a> {
    pub source: &'a Value,
    /// The declaration id the manifest must carry.
    pub expected: Option<&'a str>,
    /// The known record being refreshed (only it may carry the official id).
    pub trusted: Option<&'a str>,
}

/// Stages the marketplace directory: the source tree (if any) plus the
/// canonical `marketplace.json`, committed with `known_marketplaces.json`.
async fn stage(
    ports: &Ports<'_>,
    name: &str,
    source_root: Option<&Path>,
    manifest: &Value,
) -> Result<Activation> {
    crate::failure::check(ports.cancel)?;
    let target = crate::store::market_dir(ports.storage, name);
    let authority = ports.storage.join(crate::store::KNOWN_MARKETPLACES);
    let input = activation::Input {
        target: &target,
        source: source_root,
        authority: Some(&authority),
        cancel: ports.cancel,
    };
    activation::activate(input, |staged| async move {
        crate::store::write_json(&staged.join(crate::store::MARKETPLACE_FILE), manifest).await?;
        Ok(())
    })
    .await
}

fn guard(loaded: &Loaded, add: &Add<'_>) -> Result<()> {
    let name = loaded.manifest.name.as_str();
    if name == MARKETPLACE && Some(name) != add.trusted {
        bail!(
            "Cannot add a marketplace named \"{name}\": that id is reserved for the official marketplace."
        );
    }
    if let Some(expected) = add.expected.filter(|e| *e != name) {
        bail!("Marketplace declaration id mismatch: expected {expected}, received {name}");
    }
    if add.trusted == Some(MARKETPLACE) && name != MARKETPLACE {
        bail!("Official marketplace source must provide {MARKETPLACE}, received {name}");
    }
    Ok(())
}

/// Node `addMarketplace`: the manifest is fetched without persisting, guarded,
/// then the snapshot and the authority record commit together.
pub async fn add(ports: &Ports<'_>, add: Add<'_>) -> Result<Value> {
    crate::failure::check(ports.cancel)?;
    let mut loaded: Option<Loaded> = None;
    let mut upsert: Option<crate::known::Upsert> = None;
    let mut staged: Option<Activation> = None;
    let result = async {
        let current = loaded.insert(marketplace_source::load(add.source, ports).await?);
        crate::failure::check(ports.cancel)?;
        guard(current, &add)?;
        let name = current.manifest.name.clone();
        let official = name == MARKETPLACE;
        let persisted = if official {
            let merged =
                crate::official_partition::write_cdn(ports.storage, &current.manifest.raw).await?;
            marketplace_source::parse_required(&merged)?
        } else {
            current.manifest.clone()
        };
        // 旧流程先删目标再复制，刷新失败会丢失最后成功的快照；这里暂存完整后一次改名激活。
        if let Some(root) = current.source_root.clone() {
            staged = Some(stage(ports, &name, Some(&root), &persisted.raw).await?);
        } else if !official {
            staged = Some(stage(ports, &name, None, &current.manifest.raw).await?);
        }
        crate::failure::check(ports.cancel)?;
        let now = crate::store::now();
        let mut record = json!({"id":name,"source":add.source,"name":name,"addedAt":now,
            "lastUpdated":now,"pluginCount":persisted.plugins.len()});
        if let Some(description) = &current.manifest.description
            && !description.is_empty()
        {
            record["description"] = description.clone().into();
        }
        if let Some(activation) = &staged {
            record["cacheTransactionId"] = activation.transaction_id.clone().into();
        }
        let authority = upsert.insert(crate::known::upsert(ports.storage, record.clone()).await?);
        if staged.is_some() {
            crate::failure::check(ports.cancel)?;
        }
        // 权威状态落盘后才进入不可取消的提交尾声。
        if let Some(activation) = staged.take() {
            activation.finalize().await;
        }
        authority.finalize();
        Ok(record)
    }
    .await;
    let cleanup_dir = loaded.as_ref().and_then(|l| l.cleanup.clone());
    let result = match result {
        Ok(record) => Ok(record),
        Err(error) => {
            let mut rollback: Option<String> = None;
            if let Some(authority) = &mut upsert
                && let Err(e) = authority.rollback(ports.storage).await
            {
                rollback = Some(e.to_string());
            }
            if let Some(activation) = staged.take() {
                if rollback.is_none() {
                    if let Err(e) = activation.rollback().await {
                        rollback = Some(e.to_string());
                    }
                } else {
                    // 权威状态无法恢复时保留它指向的新快照，避免跨代状态。
                    activation.finalize().await;
                }
            }
            Err(crate::failure::append_cleanup(error, rollback))
        }
    };
    crate::store::cleanup(cleanup_dir.as_deref()).await;
    result
}

/// Node `updateMarketplace({marketplace: id})`: failures are recorded on the
/// known record unless the operation was cancelled.
pub async fn refresh(ports: &Ports<'_>, id: &str) -> Result<Vec<Value>> {
    crate::market::ensure_defaults(ports.storage).await?;
    let known = crate::market::known(ports.storage).await?;
    let selected: Vec<&Value> = known.iter().filter(|r| r["id"] == id).collect();
    if selected.is_empty() {
        bail!("Marketplace not found: {id}");
    }
    let mut updated = vec![];
    for record in selected {
        crate::failure::check(ports.cancel)?;
        let request = Add {
            source: &record["source"],
            expected: None,
            trusted: record["id"].as_str(),
        };
        match add(ports, request).await {
            Ok(record) => updated.push(record),
            Err(error) => {
                // 取消是当前操作的控制流，不是市场健康状态，不能持久化成刷新失败。
                if ports.cancel.is_cancelled() {
                    return Err(error);
                }
                let diagnostic = crate::diagnose::validation(&error, record["id"].as_str());
                let failure = json!({"code":diagnostic.code,"failedAt":crate::store::now(),
                    "message":diagnostic.message});
                crate::known::persist_failure(ports.storage, id, failure).await?;
            }
        }
    }
    Ok(updated)
}

/// Node `ensureMarketplaceManifestAvailable`: a missing snapshot of a known
/// marketplace is fetched from its recorded source.
pub async fn ensure_available(ports: &Ports<'_>, id: &str) -> Result<Option<Value>> {
    crate::failure::check(ports.cancel)?;
    crate::market::ensure_defaults(ports.storage).await?;
    let known = crate::market::known(ports.storage).await?;
    let record = known.into_iter().find(|r| r["id"] == id);
    if crate::market::manifest(ports.storage, id).await?.is_some() {
        return Ok(record);
    }
    let Some(record) = record else {
        return Ok(None);
    };
    let request = Add {
        source: &record["source"],
        expected: None,
        trusted: record["id"].as_str(),
    };
    Ok(Some(add(ports, request).await?))
}
