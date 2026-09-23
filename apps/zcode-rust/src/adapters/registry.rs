//! Existing App provider files, resolved once per configuration revision.
use super::registry_rules::{resolve, validate_account};
use crate::contract::{ModelIdentity, ModelPort, ModelRegistry};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, RwLock},
};

pub(super) type ModelKey = (String, String, String);
#[derive(Default)]
pub(super) struct Snapshot {
    pub(super) signature: Vec<u8>,
    pub(super) models: BTreeMap<ModelKey, Arc<dyn ModelPort>>,
    pub(super) catalog: Vec<Value>,
    pub(super) options: Vec<Value>,
    pub(super) default: Option<ModelIdentity>,
}
pub struct Registry {
    builtin: PathBuf,
    personal: PathBuf,
    account: tokio::sync::Mutex<Option<Value>>,
    snapshot: RwLock<Snapshot>,
    pool: Arc<tokio::sync::OnceCell<reqwest::Client>>,
}
impl Registry {
    pub async fn from_env() -> Result<Option<Arc<Self>>> {
        let builtin = std::env::var_os("ZCODE_BUILTIN_PROVIDER_CONFIG_FILE");
        let personal = std::env::var_os("ZCODE_PERSONAL_PROVIDER_CONFIG_FILE");
        if builtin.is_none() && personal.is_none() {
            return Ok(None);
        }
        let registry = Arc::new(Self::new(
            builtin.context("Builtin config required")?.into(),
            personal.context("Personal config required")?.into(),
        ));
        registry.refresh(None).await?;
        Ok(Some(registry))
    }
    pub fn new(builtin: PathBuf, personal: PathBuf) -> Self {
        Self {
            builtin,
            personal,
            account: Default::default(),
            snapshot: Default::default(),
            pool: Default::default(),
        }
    }
}
#[async_trait::async_trait]
impl ModelRegistry for Registry {
    async fn received_account(&self) -> Option<Value> {
        self.account.lock().await.clone()
    }
    fn model_options(&self) -> Vec<Value> {
        self.snapshot.read().unwrap().options.clone()
    }
    fn catalog(&self) -> Vec<Value> {
        self.snapshot.read().unwrap().catalog.clone()
    }
    fn default_selection(&self) -> Option<ModelIdentity> {
        self.snapshot.read().unwrap().default.clone()
    }
    fn resolve(&self, s: &ModelIdentity) -> Result<Arc<dyn ModelPort>> {
        self.snapshot
            .read()
            .unwrap()
            .models
            .get(&(
                s.provider_id.clone(),
                s.model_id.clone(),
                s.reasoning_level.clone(),
            ))
            .cloned()
            .context("Selected model is unavailable")
    }
    async fn refresh(&self, update: Option<Value>) -> Result<bool> {
        // 配置刷新共用一个串行入口；迟到的文件读取不能覆盖新账号事实。
        let mut account = self.account.lock().await;
        if let Some(next) = update {
            validate_account(&next)?;
            *account = Some(next);
        }
        let builtin = tokio::fs::read(&self.builtin)
            .await
            .context("Cannot read builtin provider config")?;
        let personal = match tokio::fs::read(&self.personal).await {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                b"{\"schemaVersion\":1,\"config\":{}}".to_vec()
            }
            Err(_) => anyhow::bail!("Cannot read personal provider config"),
        };
        ensure!(
            builtin.len() <= 8 * 1024 * 1024 && personal.len() <= 8 * 1024 * 1024,
            "Provider config exceeds limit"
        );
        let mut hash = Sha256::new();
        hash.update(&builtin);
        hash.update(&personal);
        hash.update(serde_json::to_vec(&*account)?);
        let signature = hash.finalize().to_vec();
        if self.snapshot.read().unwrap().signature == signature {
            return Ok(false);
        }
        let builtin: Value =
            serde_json::from_slice(&builtin).context("Invalid builtin provider config")?;
        let personal: Value =
            serde_json::from_slice(&personal).context("Invalid personal provider config")?;
        ensure!(
            builtin["schemaVersion"] == 1 && personal["schemaVersion"] == 1,
            "Unsupported provider schema"
        );
        let path = std::path::absolute(&self.builtin)?;
        let source = format!("{:x}", Sha256::digest(path.to_string_lossy().as_bytes()));
        let revision = format!(
            "zcode-builtin:{}:{source}",
            builtin["revision"]
                .as_u64()
                .context("Invalid builtin revision")?
        );
        if let Some(account) = &*account
            && account["basedOnZCodeBuiltinRevision"] != revision
        {
            return Ok(false);
        }
        let pool = self.pool.clone();
        let account_value = account.clone().unwrap_or_else(|| json!({}));
        let mut next = tokio::task::spawn_blocking(move || {
            resolve(
                &builtin["config"],
                &personal["config"],
                &account_value,
                pool,
            )
        })
        .await??;
        next.signature = signature;
        *self.snapshot.write().unwrap() = next;
        Ok(true)
    }
}
