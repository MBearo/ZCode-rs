// 语义复制自 apps/zcode-cli/packages/bootstrap/src/app/standalone-account-provider-runtime.ts（e80be55974）的键名与
// 手工 key。Team Plan 取 App 的 TS 账号栈（packages/provider 的 account-provider-resolution.ts，staging 699b62b8d6，
// 工作线 F47）；OAuth 的身份（profile id）取 packages/services/src/oauth/repo/oauthCredentialRepo.ts 的
// `loadUserProfile`（工作线 F49 起团队、F50 起个人也用它）。
//! 官方账号模型（docs/specs/rust-cli/platform.md §5、§9）。
//!
//! - overlay：内置配置里 `access.type = zhipu-account`、`mode = individual-coding-plan`、有 `accountType`
//!   与内置模型的 provider（个人 Coding Plan）：手工 key（`auth-source = manual`）有 key 时授权；其余照 App——有
//!   profile id，个人订阅 available 时授权（`personal/`，§5.7）。团队 provider 有可用的团队项目时授权（`team/`，
//!   模型按团队项目平铺，§9.3）；其余账号 provider 一律不授权——缺少成员不能表达“已断开”，必须显式
//!   `entitled: false`。不写 `states.current`、不读 App 的连接选择（工作线 F49）。
//! - 每请求鉴权：手工 key 取存好的 key；OAuth 登录（App 或 `zcode login`）按 profile id 用 access token 换短期
//!   项目令牌（`auth.rs`、`token/`）。
//!
//! 工作线 F50（用户决定 2026-10-21，账号以 App 为准）：以前照 Node 独立 CLI，OAuth 登录也要
//! `account-provider:<id>:identity` 才授权，只在 App 里登录过的用户个人套餐不可用；现在 OAuth 只看 App 的 profile id，
//! `identity` / `auth-source` 只表示手工 key。

use std::collections::HashMap;

use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::model::ModelError;
use serde_json::{Map, Value, json};

use rust_cli_base::js::string::trim as js_trim;

use crate::credentials::Credentials;
use crate::personal::PersonalAccount;
use crate::providers::resolve::AccountLayer;
use crate::providers::rules::{array, ids, text};
use crate::team::{TeamAccount, TeamPlan};
use crate::token::Family;

pub(crate) const INDIVIDUAL_CODING_PLAN: &str = "individual-coding-plan";
pub(crate) const TEAM_CODING_PLAN: &str = "team-coding-plan";
/// bigmodel 旧版本可能把它误写进 OAuth access token（App 的防御）。
pub(crate) const ZCODE_JWT_KEY: &str = "zcodejwttoken";

/// 账号连接的身份键（Node `standaloneAccountIdentityCredentialKey`）。
pub(crate) fn identity_key(provider: &str) -> String {
    format!("account-provider:{}:identity", provider.trim())
}

/// 连接来源键：`manual`（手工 key）或 `oauth`。
pub(crate) fn source_key(provider: &str) -> String {
    format!("account-provider:{provider}:auth-source")
}

/// 手工 key 的存放键（Node `standaloneAccountProviderCredentialKey`）。
pub(crate) fn manual_key(provider: &str, identity: &str) -> String {
    format!(
        "account-provider:coding-plan:{}:account:{}:api-key",
        provider.trim(),
        encode_uri_component(identity.trim())
    )
}

/// OAuth access token 的键（`SHARED_ZCODE_CREDENTIAL_KEYS`）。
pub(crate) fn oauth_key(family: &str) -> &'static str {
    if family == "zai" {
        "oauth:zai:access_token"
    } else {
        "oauth:bigmodel:access_token"
    }
}

/// JS `encodeURIComponent`：保留 `A-Z a-z 0-9 - _ . ! ~ * ' ( )`，其余按 UTF-8 百分号编码。
pub(crate) fn encode_uri_component(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// 独立 CLI 支持的账号 provider（个人 Coding Plan）。
pub(crate) struct CodingPlanProvider {
    pub provider: String,
    pub family: String,
    /// 第一个内置模型（登录后写成默认选择）。
    pub model: String,
}

/// 内置配置里的个人 Coding Plan provider（Node `readStandaloneCodingPlanCatalog`）。
pub(crate) fn coding_plan_providers(builtin: &Value) -> Vec<CodingPlanProvider> {
    array(&builtin["providerConfigRules"]["providerRules"])
        .iter()
        .filter_map(|rule| {
            let access = &rule["config"]["access"];
            let family = text(access, "accountType");
            let model = ids(&rule["config"]["builtinModelIds"])
                .into_iter()
                .map(|id| id.trim().to_owned())
                .find(|id| !id.is_empty())?;
            (access["type"] == "zhipu-account"
                && access["mode"] == INDIVIDUAL_CODING_PLAN
                && !family.is_empty())
            .then(|| CodingPlanProvider {
                provider: text(rule, "providerId").to_owned(),
                family: family.to_owned(),
                model,
            })
        })
        .collect()
}

/// 内置配置里的一个账号 provider（`access.type = zhipu-account`）。
pub(crate) struct AccountProvider {
    pub provider: String,
    pub family: Option<Family>,
    pub mode: String,
}

pub(crate) fn account_providers(builtin: &Value) -> Vec<AccountProvider> {
    array(&builtin["providerConfigRules"]["providerRules"])
        .iter()
        .filter(|rule| rule["config"]["access"]["type"] == "zhipu-account")
        .filter(|rule| !text(rule, "providerId").is_empty())
        .map(|rule| {
            let access = &rule["config"]["access"];
            AccountProvider {
                provider: text(rule, "providerId").to_owned(),
                family: Family::parse(text(access, "accountType")),
                mode: text(access, "mode").to_owned(),
            }
        })
        .collect()
}

/// 每个团队 provider 的账号输入（platform.md §9.1）：access token、profile id 与 zcodejwttoken。
pub(crate) fn team_accounts(
    builtin: &Value,
    credentials: Option<&Credentials>,
) -> Vec<TeamAccount> {
    account_providers(builtin)
        .into_iter()
        .filter(|candidate| candidate.mode == TEAM_CODING_PLAN)
        .filter_map(|candidate| {
            let family = candidate.family?;
            let present = |key: &str| credentials.and_then(|c| c.present(key));
            Some(TeamAccount {
                provider: candidate.provider,
                family,
                identity: credentials.and_then(|c| profile_id(c, family)),
                token: present(oauth_key(family.as_str())),
                zcode_jwt: present(ZCODE_JWT_KEY),
            })
        })
        .collect()
}

/// 个人 Coding Plan provider 的 OAuth 账号输入（platform.md §5.2、§5.7）：profile id 与 access token。来源是 manual 的
/// provider 不在这里（手工 key 不查订阅）；凭据读不到时没有。
pub(crate) fn personal_accounts(
    builtin: &Value,
    credentials: Option<&Credentials>,
) -> Vec<PersonalAccount> {
    let Some(credentials) = credentials else {
        return Vec::new();
    };
    coding_plan_providers(builtin)
        .into_iter()
        .filter(|candidate| !manual(credentials, &candidate.provider))
        .filter_map(|candidate| {
            let family = Family::parse(&candidate.family)?;
            Some(PersonalAccount {
                identity: profile_id(credentials, family),
                token: credentials.present(oauth_key(family.as_str())),
                provider: candidate.provider,
                family,
            })
        })
        .collect()
}

/// overlay 的输入：凭据（读不到或损坏时为空）、这一轮每个个人 provider 的订阅结论（`personal/`）与每个团队 provider
/// 可用的团队项目。
pub(crate) struct AccountView<'a> {
    pub credentials: Option<&'a Credentials>,
    pub personal: &'a HashMap<String, bool>,
    pub teams: &'a HashMap<String, Vec<TeamPlan>>,
}

/// 账号 overlay。`credentials` 为空（读不到或损坏）时全部不授权（fail-closed）。
pub(crate) fn overlay(builtin: &Value, view: &AccountView<'_>) -> AccountLayer {
    let supported = coding_plan_providers(builtin);
    let mut providers = Map::new();
    for candidate in account_providers(builtin) {
        let id = candidate.provider.as_str();
        let individual = supported.iter().any(|supported| supported.provider == id);
        let entitled = match view.credentials {
            Some(credentials) if individual && manual(credentials, id) => {
                manual_authorized(credentials, id)
            }
            // OAuth（App 或 `zcode login`）照 App：个人订阅的结论（F50，以前照 Node 要连接身份）。
            Some(_) if individual => view.personal.get(id).copied().unwrap_or(false),
            Some(_) if candidate.mode == TEAM_CODING_PLAN => {
                view.teams.get(id).is_some_and(|plans| !plans.is_empty())
            }
            _ => false,
        };
        providers.insert(
            id.to_owned(),
            json!({"access": {"type": "zhipu-account", "entitled": entitled}}),
        );
    }
    AccountLayer {
        providers,
        states: Map::new(),
    }
}

/// `oauth:<family>:user_info` 的 profile id（App `OAuthCredentialRepo.loadUserProfile` 的 `id`，platform.md §9.8）。
/// 去空白后为空时算没有；给出的是原值（App 的团队项目位置键用原值）。
pub(crate) fn profile_id(credentials: &Credentials, family: Family) -> Option<String> {
    let raw = credentials.present(&format!("oauth:{}:user_info", family.as_str()))?;
    let parsed: Value = serde_json::from_str(raw.expose()).ok()?;
    let text = |name: &str| parsed.get(name).and_then(Value::as_str);
    let id =
        if let (Some(id), Some(_), Some(_)) = (text("id"), text("username"), text("displayName")) {
            id.to_owned()
        } else if family == Family::Zai && parsed.is_object() {
            // ZAI 的 user_info 也可能是后端原始用户对象（本工程与 Node 登录时这样存）。
            let id = text("user_id").unwrap_or("unknown");
            let name = text("name").map(js_trim).unwrap_or("");
            let email = text("email").unwrap_or("");
            if name.is_empty() && email.is_empty() && id == "unknown" {
                return None;
            }
            id.to_owned()
        } else {
            return None;
        };
    (!js_trim(&id).is_empty()).then_some(id)
}

/// 来源是手工 key（`auth-source = manual`，本工程与 Node 独立 CLI 的手工 key 登录）。
fn manual(credentials: &Credentials, provider: &str) -> bool {
    credentials
        .present(&source_key(provider))
        .is_some_and(|source| source.expose() == "manual")
}

/// 手工 key 连接：有身份，且有按身份存的 key。
fn manual_authorized(credentials: &Credentials, provider: &str) -> bool {
    credentials
        .present(&identity_key(provider))
        .is_some_and(|identity| {
            credentials
                .present(&manual_key(provider, identity.expose()))
                .is_some()
        })
}

/// 这次请求要用的账号凭据（Node `refreshBeforeModelRequest` 的判断部分；换取在 `auth.rs`）。
pub(crate) enum RequestAuth {
    /// 手工 key 登录：存好的 key。
    Manual(RedactedString),
    /// OAuth 登录（App 或 `zcode login`）：按 profile id 用 access token 换项目令牌。
    OAuth(OAuthLogin),
}

/// OAuth 登录的请求输入（App `loadIndividualPlanMaterial` 与 `#scope` 读的值）。
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct OAuthLogin {
    /// App 的 profile id（`oauth:<family>:user_info`）。
    pub identity: String,
    pub access_token: RedactedString,
    pub zcode_jwt: Option<RedactedString>,
}

/// 按来源选这次请求的凭据：来源 manual 用存好的 key；其余照 App，要 profile id 与 access token（F50：不再要
/// `account-provider:<id>:identity`，也不读 `auth-source = oauth`）；都没有时要求重新登录。
pub(crate) fn request_auth(
    credentials: &Credentials,
    provider: &str,
    family: &str,
    mode: &str,
) -> Result<RequestAuth, ModelError> {
    let unavailable = |message: String| ModelError::Authentication { message };
    if mode != INDIVIDUAL_CODING_PLAN {
        return Err(unavailable(format!(
            "account provider {provider} ({mode}) is not available without the ZCode app"
        )));
    }
    if manual(credentials, provider) {
        let identity = credentials
            .present(&identity_key(provider))
            .ok_or_else(|| unavailable(format!("account provider {provider} is not logged in")))?;
        return credentials
            .present(&manual_key(provider, identity.expose()))
            .map(RequestAuth::Manual)
            .ok_or_else(|| unavailable(format!("account provider {provider} has no API key")));
    }
    let login = Family::parse(family).and_then(|family| {
        Some(OAuthLogin {
            identity: profile_id(credentials, family)?,
            access_token: credentials.present(oauth_key(family.as_str()))?,
            zcode_jwt: credentials.present(ZCODE_JWT_KEY),
        })
    });
    login.map(RequestAuth::OAuth).ok_or_else(|| {
        unavailable(format!(
            "project_token_login_required: account provider {provider} needs a login \
             (run `zcode login`)"
        ))
    })
}

#[cfg(test)]
#[path = "account_tests.rs"]
mod tests;
