// 语义复制自 packages/services/src/usage-stats/（staging 420d27c977）：providers/bigmodelUsageQuotaProvider.ts 的
// `resolveAuthorization`（Coding Plan 分支）、`createBigModelUsageHeaders`、`buildQuotaLimitUrl`、
// `isSuccessfulBigModelEnvelope`、`readSuccessfulBigModelMonitorData`，与 packages/shared 的 `BUILTIN_MODEL_PROVIDER_IDS`、
// `MODEL_PROVIDER_FAMILY_SPECS` 的顺序。
//! 套餐用量（docs/specs/rust-cli/platform.md §11）：Puffin `/usage` 的计划清单、额度与历史。
//!
//! - [`Platform::usage_view`]：registry 的快照里每个 family 的个人 / 团队 provider（不联网，§11.2）；
//! - [`Platform::plan_quota`]：订阅摘要、quota/limit 与 MCP 额度（`quota.rs`、`mcp.rs`，§11.3、§11.4）；
//! - [`Platform::plan_history`]：activity、usage-detail 与解码速度（`history.rs`，§11.5）。
//!
//! 凭据与模型请求同一份（`auth.rs`）；额度与历史不落盘、不缓存（一分钟的缓存在 Puffin host）。

mod history;
mod mcp;
mod quota;
mod status;
mod time;

use std::sync::Arc;

use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::model::ModelConnection;
use rust_cli_kernel_api::provider::CatalogConnection;
use rust_cli_net::HeaderValue;
use serde_json::Value;
use url::Url;

pub use history::{
    ActivitySnapshot, HealthSnapshot, HistoryRange, HistorySnapshot, ModelUsage, ToolUsage,
};
pub use quota::{PlanStatus, Quota, QuotaLimit, QuotaSnapshot, UsageDetail};
pub use status::{AccountPlan, FamilyAccount};

use crate::Platform;
use crate::account::{self, INDIVIDUAL_CODING_PLAN, TEAM_CODING_PLAN, ZCODE_JWT_KEY};
use crate::credentials::Credentials;
use crate::token::AccountOrigins;
use crate::token::http::{Http, ORGANIZATION_HEADER, PROJECT_HEADER};
use rust_cli_base::account::Family;

/// 套餐用量的响应体上限（D-U-5）：一年的活动与 30 天的明细可能超过账号接口的 64 KiB。
const MAX_USAGE_BYTES: usize = 1024 * 1024;

/// quota/limit 的路径（App `BIGMODEL_QUOTA_PATH`）；monitor 的其他接口由它换路径。
const QUOTA_PATH: &str = "/api/monitor/usage/quota/limit";

/// 六个内置账号 provider（App `BUILTIN_MODEL_PROVIDER_IDS`）：Puffin 用它判断当前模型是不是官方的。
pub const OFFICIAL_PROVIDERS: [&str; 6] = [
    "account:zai-individual-coding-plan",
    "account:zai-team-coding-plan",
    "account:zai-start-plan",
    "account:bigmodel-individual-coding-plan",
    "account:bigmodel-team-coding-plan",
    "account:bigmodel-start-plan",
];

/// family 的顺序（App `MODEL_PROVIDER_FAMILY_SPECS`）。
const FAMILIES: [Family; 2] = [Family::Zai, Family::Bigmodel];

/// 计划清单的原料（§11.2）：每个 family 的个人与团队 provider，加上快照里每个 provider 的显示名与密钥管理页。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageView {
    pub families: Vec<FamilyPlans>,
    pub providers: Vec<ProviderFacts>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FamilyPlans {
    pub family: Family,
    pub individual: Option<IndividualProvider>,
    pub team: Option<TeamProvider>,
}

/// 个人 Coding Plan 的 provider：快照里有它的模板时已授权。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndividualProvider {
    pub provider: String,
    pub entitled: bool,
}

/// 团队 Coding Plan 的 provider 与可用的团队项目（快照的顺序）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamProvider {
    pub provider: String,
    pub teams: Vec<CatalogConnection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderFacts {
    pub provider: String,
    pub name: String,
    /// 生效配置的 `access.apiKeyManagementUrl`。
    pub key_url: Option<String>,
}

/// 问额度或历史的计划：个人是 provider 与 family；团队另带团队项目。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PlanTarget {
    pub provider: String,
    pub family: Family,
    pub team: Option<ModelConnection>,
}

impl PlanTarget {
    fn mode(&self) -> &'static str {
        if self.team.is_some() {
            TEAM_CODING_PLAN
        } else {
            INDIVIDUAL_CODING_PLAN
        }
    }
}

/// 一个计划的账号输入（§11.3 第 1、7 步：前后不同时这一次作废，App `assertCurrent`）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Login {
    identity: Option<String>,
    token: Option<RedactedString>,
    zcode_jwt: Option<RedactedString>,
    active: Option<RedactedString>,
}

impl Login {
    fn read(credentials: &Credentials, family: Family) -> Self {
        Self {
            identity: account::profile_id(credentials, family),
            token: credentials.present(account::oauth_key(family.as_str())),
            zcode_jwt: credentials.present(ZCODE_JWT_KEY),
            active: credentials.present("oauth:active_provider"),
        }
    }
}

impl Platform {
    /// 计划清单的原料（§11.2）：内置配置的账号 provider 与当前快照，不联网。
    pub async fn usage_view(&self) -> UsageView {
        let builtin = self.registry.builtin_config().await.unwrap_or_default();
        let current = self.registry.current().await;
        let templates = &current.snapshot.templates;
        let providers = account::account_providers(&builtin);
        let find = |family: Family, mode: &str| {
            providers
                .iter()
                .find(|p| p.family == Some(family) && p.mode == mode)
                .map(|p| p.provider.clone())
        };
        let families = FAMILIES
            .into_iter()
            .map(|family| FamilyPlans {
                family,
                individual: find(family, INDIVIDUAL_CODING_PLAN).map(|provider| {
                    let entitled = templates.iter().any(|t| t.provider == provider);
                    IndividualProvider { provider, entitled }
                }),
                team: find(family, TEAM_CODING_PLAN).map(|provider| {
                    let mut teams: Vec<CatalogConnection> = Vec::new();
                    for connection in templates
                        .iter()
                        .filter(|t| t.provider == provider)
                        .filter_map(|t| t.connection.as_ref())
                    {
                        if !teams.iter().any(|known| known.id == connection.id) {
                            teams.push(connection.clone());
                        }
                    }
                    TeamProvider { provider, teams }
                }),
            })
            .collect();
        let mut facts: Vec<ProviderFacts> = Vec::new();
        for template in templates {
            if !facts.iter().any(|f| f.provider == template.provider) {
                facts.push(ProviderFacts {
                    provider: template.provider.clone(),
                    name: template.provider_name.clone(),
                    key_url: template.key_url.clone(),
                });
            }
        }
        UsageView {
            families,
            providers: facts,
        }
    }

    /// 这个计划的额度（§11.3）。
    pub async fn plan_quota(&self, target: &PlanTarget) -> Result<QuotaSnapshot, String> {
        quota::fetch(&self.usage_context(), target).await
    }

    /// 这个计划在 `range` 里的历史（§11.5）。
    pub async fn plan_history(
        &self,
        target: &PlanTarget,
        range: HistoryRange,
    ) -> Result<HistorySnapshot, String> {
        history::fetch(&self.usage_context(), target, range).await
    }

    fn usage_context(&self) -> Context<'_> {
        Context {
            platform: self,
            http: Http::with_limit(self.services.egress.clone(), MAX_USAGE_BYTES),
            origins: &self.services.origins,
            clock: self.services.clock.clone(),
        }
    }
}

/// 一次查询要的东西。
struct Context<'a> {
    platform: &'a Platform,
    http: Http,
    origins: &'a AccountOrigins,
    clock: Arc<dyn rust_cli_kernel_api::clock::Clock>,
}

impl Context<'_> {
    async fn login(&self, family: Family) -> Result<Login, String> {
        let credentials = self
            .platform
            .credentials
            .read()
            .await
            .map_err(|error| error.to_string())?;
        Ok(Login::read(&credentials, family))
    }

    /// 与模型请求同一份凭据（§5.3、§9.5）：手工 key 或项目令牌。
    async fn credential(&self, target: &PlanTarget) -> Result<RedactedString, String> {
        self.platform
            .auth
            .credential(
                (&target.provider, target.family.as_str(), target.mode()),
                target.team.as_ref(),
                None,
            )
            .await
            .map_err(|error| error.to_string())
    }

    /// quota/limit 的地址（App `resolveQuotaUrlFromEnv ?? resolveAccountProviderQuotaUrl`）。
    fn quota_url(&self, family: Family) -> Result<Url, String> {
        let text = match &self.origins.usage_quota_url {
            Some(url) => url.clone(),
            None => {
                let origin = match family {
                    Family::Zai => &self.origins.zai_api,
                    Family::Bigmodel => &self.origins.bigmodel_api,
                };
                format!("{origin}{QUOTA_PATH}")
            }
        };
        Url::parse(&text).map_err(|_| format!("invalid usage quota URL: {text}"))
    }
}

/// monitor 接口的头（App `createBigModelUsageHeaders`）：凭据原样（不加 Bearer）；团队另带组织与项目。
fn monitor_headers(
    credential: &RedactedString,
    team: Option<&ModelConnection>,
) -> Vec<(&'static str, HeaderValue)> {
    let mut headers = vec![("authorization", HeaderValue::Secret(credential.clone()))];
    if let Some(team) = team {
        headers.push((
            ORGANIZATION_HEADER,
            HeaderValue::Plain(team.organization_id.clone()),
        ));
        headers.push((PROJECT_HEADER, HeaderValue::Plain(team.project_id.clone())));
    }
    headers
}

/// JS `URLSearchParams.set`：换掉第一个同名参数并删掉其余的，没有时加在最后；按表单编码重写整个查询。
fn set_param(url: &mut Url, name: &str, value: &str) {
    let mut pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    match pairs.iter().position(|(key, _)| key == name) {
        Some(at) => {
            pairs[at].1 = value.to_owned();
            let mut index = 0;
            pairs.retain(|(key, _)| {
                index += 1;
                key != name || index - 1 == at
            });
        }
        None => pairs.push((name.to_owned(), value.to_owned())),
    }
    url.query_pairs_mut().clear().extend_pairs(pairs);
}

/// `url.pathname.replace(/<from>$/, to)`：路径以 `from` 结尾时换掉那一段，否则不变。
fn replace_path_end(url: &mut Url, from: &str, to: &str) {
    if let Some(head) = url.path().strip_suffix(from) {
        let path = format!("{head}{to}");
        url.set_path(&path);
    }
}

/// App `isSuccessfulBigModelEnvelope`：`success` 不是 false，`code` 缺省、null、0 或 200。
fn successful(payload: &Value) -> bool {
    let code_ok = match payload.get("code") {
        None | Some(Value::Null) => true,
        Some(Value::Number(number)) => matches!(number.as_f64(), Some(n) if n == 0.0 || n == 200.0),
        Some(_) => false,
    };
    payload.get("success") != Some(&Value::Bool(false)) && code_ok
}

/// App `hasExplicitBigModelSuccessSignal`：`success` 是 true，或 `code` 是 0 / 200。
fn explicitly_successful(payload: &Value) -> bool {
    payload.get("success") == Some(&Value::Bool(true))
        || matches!(payload.get("code").and_then(Value::as_f64), Some(n) if n == 0.0 || n == 200.0)
}

/// App `readBigModelEnvelopeMessage`：`msg`，其次 `message`（去空白）。
fn envelope_message(payload: &Value) -> String {
    ["msg", "message"]
        .iter()
        .filter_map(|name| payload.get(*name).and_then(Value::as_str))
        .map(str::trim)
        .find(|text| !text.is_empty())
        .unwrap_or_default()
        .to_owned()
}

/// JS `toFiniteNumber`：数字，或去空白后非空、能读成有限数的字符串。
fn finite(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64().filter(|n| n.is_finite()),
        Value::String(text) if !text.trim().is_empty() => {
            text.trim().parse::<f64>().ok().filter(|n| n.is_finite())
        }
        _ => None,
    }
}

/// 只认 JSON 数字（App 的 `typeof x === "number"`）。
fn number(value: &Value) -> Option<f64> {
    value.as_f64().filter(|n| n.is_finite())
}

/// `x?.trim()` 的非空结果（只认字符串）。
fn text(value: &Value) -> Option<&str> {
    value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
