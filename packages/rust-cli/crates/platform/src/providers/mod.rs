//! Provider registry（docs/specs/rust-cli/platform.md §3）：内置 + 个人配置 + 账号 overlay → 模型目录与
//! 端点模板。
//!
//! 来源与 Node 独立 CLI 相同：内置配置是随包的一份（或 `ZCODE_BUILTIN_PROVIDER_CONFIG_FILE`），个人配置是
//! `~/.zcode/v2/provider_config.json`，账号 overlay 由 `credentials.json`、个人订阅与团队列表、权益算出（`account.rs`、
//! `personal/`、`team/`，工作线 F47、F49、F50）；团队 provider 的模型按可用的团队项目平铺（`catalog.rs`，platform.md
//! §9.3）。
//!
//! 热更新：没有后台轮询任务。每次取目录或端点时先比三个文件的元数据（`ChangeGate`，元数据不变时每分钟
//! 兜底读一次，与 Host 的轮询规则相同，docs/specs/rust-provider-config-poll.md），有变化才读；读到的内容
//! 签名没变就沿用上一份快照，变了才重新解析并把版本加一。个人订阅与团队查询同时进行，途中账号输入变了，这一轮
//! 丢弃：不记录元数据、不发布（platform.md §5.7、§9.7）。
//!
//! 并发：不持锁跨 await。每次读取先取一个递增序号，只有序号更大的结果能覆盖已发布的快照，迟到的旧读取
//! 不会盖掉新读取（与 net 的出口策略热替换同一规则）。

pub(crate) mod catalog;
pub(crate) mod resolve;
pub(crate) mod rules;
pub(crate) mod site;
pub(crate) mod tool_search;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use std::collections::{BTreeMap, HashMap};

use futures_util::future::{join, join_all};
use rust_cli_base::include_lf;
use rust_cli_fs::{ChangeGate, FileStamp};
use rust_cli_kernel_api::model::ModelConnection;
use rust_cli_kernel_api::provider::CatalogConnection;
use rust_cli_paths::StoragePath;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::account::{self, AccountView};
use crate::credentials::{CredentialStore, Credentials};
use crate::personal::PersonalPlans;
use crate::team::{TeamAccount, TeamPlan, TeamPlans};
use catalog::Snapshot;

/// 元数据不变时两次读取之间的最长间隔（Host `FULL_READ_EVERY`）。
const FULL_READ_EVERY: Duration = Duration::from_secs(60);
/// 单个配置文件的上限（旧 Rust CLI 的 registry 相同）。
const MAX_CONFIG_BYTES: usize = 8 * 1024 * 1024;
/// 随包的内置 Provider 配置：复制自仓库的 `config/provider/zcode-builtin.json`（测试校验两者相同）。
pub(crate) const BUNDLED_BUILTIN: &str = include_lf!("../../assets/zcode-builtin.json");
/// 个人配置不存在时的内容（Node 同样按空配置处理）。
const EMPTY_PERSONAL: &[u8] = br#"{"schemaVersion":1,"config":{}}"#;

/// 已发布的一份解析结果。
#[derive(Debug, Default)]
pub(crate) struct Current {
    pub revision: u64,
    pub snapshot: Snapshot,
    signature: Vec<u8>,
}

struct Published {
    seq: u64,
    current: Arc<Current>,
}

/// 账号的联网查询：个人订阅（F50）与团队列表、权益（F47、F49）。
pub(crate) struct Plans {
    pub personal: Arc<PersonalPlans>,
    pub teams: Arc<TeamPlans>,
}

pub(crate) struct Registry {
    builtin: Option<StoragePath>,
    personal: StoragePath,
    credentials: CredentialStore,
    plans: Plans,
    gate: Mutex<ChangeGate>,
    published: Mutex<Published>,
    next_seq: AtomicU64,
}

/// 读到的三份输入。
struct Inputs {
    builtin: Vec<u8>,
    personal: Vec<u8>,
    account: Value,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // 持锁的代码不 panic、不 await；中毒时数据仍然一致，继续使用。
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Registry {
    pub fn new(
        builtin: Option<StoragePath>,
        personal: StoragePath,
        credentials: CredentialStore,
        plans: Plans,
    ) -> Self {
        Self {
            builtin,
            personal,
            credentials,
            plans,
            gate: Mutex::new(ChangeGate::new(FULL_READ_EVERY)),
            published: Mutex::new(Published {
                seq: 0,
                current: Arc::new(Current::default()),
            }),
            next_seq: AtomicU64::new(1),
        }
    }

    /// 当前快照；文件有变化时先重新读取。
    pub async fn current(&self) -> Arc<Current> {
        let mut stamps = vec![
            FileStamp::of(self.personal.as_path()).await,
            FileStamp::of(self.credentials.file().as_path()).await,
        ];
        if let Some(builtin) = &self.builtin {
            stamps.push(FileStamp::of(builtin.as_path()).await);
        }
        let now = Instant::now();
        let fresh = lock(&self.published).current.revision > 0;
        if fresh && !lock(&self.gate).should_read(&stamps, now) {
            return lock(&self.published).current.clone();
        }
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        let inputs = match self.read().await {
            Ok(inputs) => inputs,
            Err(error) => {
                // 读失败不记录元数据，下次照常重读；已有的快照继续生效。
                tracing::warn!(%error, "cannot read the provider configuration");
                return lock(&self.published).current.clone();
            }
        };
        lock(&self.gate).record(stamps, now);
        let signature = signature(&inputs);
        if lock(&self.published).current.signature == signature {
            return lock(&self.published).current.clone();
        }
        let snapshot = match rust_cli_base::blocking(move || parse(&inputs)).await {
            Ok(Ok(snapshot)) => snapshot,
            Ok(Err(error)) => {
                tracing::warn!(%error, "invalid provider configuration");
                Snapshot::default()
            }
            Err(error) => {
                tracing::warn!(%error, "resolving the provider configuration failed");
                return lock(&self.published).current.clone();
            }
        };
        let mut published = lock(&self.published);
        if seq > published.seq && published.current.signature != signature {
            published.seq = seq;
            published.current = Arc::new(Current {
                revision: published.current.revision + 1,
                snapshot,
                signature,
            });
        }
        published.current.clone()
    }

    /// 已发布的快照，不比元数据、不重读（同步的模型上限查询用，修复线 F61）：配置的变化由下一次 [`Self::current`]
    /// （取目录或端点）发现，所以最多晚一步。还没读过时是空快照。
    pub fn published(&self) -> Arc<Current> {
        lock(&self.published).current.clone()
    }

    /// 内置配置的 `config`（登录时找个人 Coding Plan provider）。
    pub async fn builtin_config(&self) -> Result<Value, String> {
        let bytes = self.read_builtin().await?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("builtin provider config: {error}"))?;
        Ok(value["config"].clone())
    }

    async fn read_builtin(&self) -> Result<Vec<u8>, String> {
        Ok(match &self.builtin {
            None => BUNDLED_BUILTIN.as_bytes().to_vec(),
            Some(file) => rust_cli_fs::read_optional(file.as_path())
                .await
                .map_err(|error| format!("builtin provider config: {error}"))?
                .ok_or_else(|| {
                    format!(
                        "builtin provider config {} is missing",
                        file.as_path().display()
                    )
                })?,
        })
    }

    async fn read(&self) -> Result<Inputs, String> {
        let builtin = self.read_builtin().await?;
        let personal = rust_cli_fs::read_optional(self.personal.as_path())
            .await
            .map_err(|error| format!("personal provider config: {error}"))?
            .unwrap_or_else(|| EMPTY_PERSONAL.to_vec());
        if builtin.len() > MAX_CONFIG_BYTES || personal.len() > MAX_CONFIG_BYTES {
            return Err("provider config exceeds 8 MiB".to_owned());
        }
        let builtin_value: Value = serde_json::from_slice(&builtin).unwrap_or(Value::Null);
        let builtin_config = &builtin_value["config"];
        let credentials = self.account_credentials().await;
        let accounts = account::team_accounts(builtin_config, credentials.as_ref());
        let individuals = account::personal_accounts(builtin_config, credentials.as_ref());
        // 个人订阅与团队、两个 family 都同时查（团队的 pricing、customerInfo 与订阅详情也是同时发的，platform.md
        // §5.7、§9.2）：第一次取目录的等待是各条查询的最大值，不是总和。
        let (rounds, checks) = join(
            join_all(
                accounts
                    .iter()
                    .map(|account| self.plans.teams.refresh(account)),
            ),
            join_all(
                individuals
                    .iter()
                    .map(|account| self.plans.personal.refresh(account)),
            ),
        )
        .await;
        if rounds.iter().any(|round| round.networked) || checks.iter().any(|check| check.networked)
        {
            // App `accountProviderConnectionResolver`：查询可能跨越换账号，旧身份的结果不能发布给新身份。
            let after = self.account_credentials().await;
            if account::team_accounts(builtin_config, after.as_ref()) != accounts
                || account::personal_accounts(builtin_config, after.as_ref()) != individuals
            {
                return Err("the account changed during the plan queries".to_owned());
            }
        }
        let teams: HashMap<String, Vec<TeamPlan>> = accounts
            .iter()
            .zip(rounds)
            .map(|(account, round)| {
                let plans = self.plans.teams.commit(account, round);
                (account.provider.clone(), plans)
            })
            .collect();
        let entitled: HashMap<String, bool> = individuals
            .iter()
            .zip(checks)
            .map(|(account, check)| {
                let entitled = self.plans.personal.commit(account, check);
                (account.provider.clone(), entitled)
            })
            .collect();
        let view = AccountView {
            credentials: credentials.as_ref(),
            personal: &entitled,
            teams: &teams,
        };
        let layer = account::overlay(builtin_config, &view);
        let connections = connections(&accounts, &teams);
        let account = json!({"providers": layer.providers, "connections": connections});
        Ok(Inputs {
            builtin,
            personal,
            account,
        })
    }
}

impl Registry {
    /// 凭据（读不到或损坏时为空：账号 provider 一律不授权，fail-closed，不影响其他 provider）。
    async fn account_credentials(&self) -> Option<Credentials> {
        match self.credentials.read().await {
            Ok(credentials) => Some(credentials),
            Err(error) => {
                tracing::warn!(%error, "account providers are unavailable");
                None
            }
        }
    }

    /// 团队项目已过期 / 未分配的提示（platform.md §9.2）。给了 `provider` 时只看它（不是团队 provider 时没有），
    /// 给了 `team` 时只看这个团队项目；都没给时按 provider 与列表的顺序取第一个。
    pub async fn team_notice(
        &self,
        provider: Option<&str>,
        team: Option<&ModelConnection>,
    ) -> Option<String> {
        let builtin = self.builtin_config().await.ok()?;
        let credentials = self.account_credentials().await;
        account::team_accounts(&builtin, credentials.as_ref())
            .iter()
            .filter(|account: &&TeamAccount| provider.is_none_or(|p| p == account.provider))
            .find_map(|account| {
                let reason = self.plans.teams.notice(account, team)?;
                crate::team::notice_text(reason, &self.plans.teams.manage_url(account.family))
            })
    }
}

/// 每个团队 provider 可用的团队项目（目录项的 `connection`，platform.md §9.3）。有序：它进快照的签名。
fn connections(
    accounts: &[TeamAccount],
    teams: &HashMap<String, Vec<TeamPlan>>,
) -> BTreeMap<String, Vec<CatalogConnection>> {
    accounts
        .iter()
        .filter_map(|account| {
            let plans = teams.get(&account.provider)?;
            let entries = plans
                .iter()
                .map(|plan| CatalogConnection {
                    id: plan.id.clone(),
                    vendor: account.family.display_name().to_owned(),
                    name: plan.name.clone(),
                })
                .collect();
            Some((account.provider.clone(), entries))
        })
        .collect()
}

fn signature(inputs: &Inputs) -> Vec<u8> {
    let mut hash = Sha256::new();
    hash.update(&inputs.builtin);
    hash.update([0]);
    hash.update(&inputs.personal);
    hash.update([0]);
    hash.update(inputs.account.to_string().as_bytes());
    hash.finalize().to_vec()
}

fn parse(inputs: &Inputs) -> Result<Snapshot, String> {
    let builtin: Value = serde_json::from_slice(&inputs.builtin)
        .map_err(|error| format!("builtin provider config: {error}"))?;
    let personal: Value = serde_json::from_slice(&inputs.personal)
        .map_err(|error| format!("personal provider config: {error}"))?;
    if builtin["schemaVersion"] != 1 || personal["schemaVersion"] != 1 {
        return Err("unsupported provider config schema".to_owned());
    }
    let layer = resolve::AccountLayer {
        providers: inputs.account["providers"]
            .as_object()
            .cloned()
            .unwrap_or_default(),
        states: serde_json::Map::new(),
    };
    let connections: HashMap<String, Vec<CatalogConnection>> =
        serde_json::from_value(inputs.account["connections"].clone()).unwrap_or_default();
    let resolved = resolve::resolve(&builtin["config"], &personal["config"], Some(&layer));
    Ok(catalog::snapshot(&resolved, &connections))
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
