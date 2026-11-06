//! 内置家族的账号状态（platform.md §11.4）：Settings › Models 的家族行与家族页（tui-kit.md §4.2「设置」）要的登录、账号、
//! 计划与团队。与计划清单（§11.2）同一份原料，不联网。
use rust_cli_kernel_api::provider::CatalogConnection;
use serde_json::Value;

use super::{FAMILIES, Login};
use crate::account;
use crate::credentials::Credentials;
use crate::{ListedModel, Platform};
use rust_cli_base::account::Family;

/// 家族现在怎么为请求付费：看新会话的缺省模型在它的哪个 provider 上；都不在时算个人的。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AccountPlan {
    Start,
    #[default]
    Individual,
    /// `teams` 里第几个团队项目。
    Team(usize),
}

/// 一个内置家族的账号状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FamilyAccount {
    pub family: Family,
    /// OAuth 登录（profile id 与 access token 都在），或个人 provider 有手工 key。
    pub signed_in: bool,
    /// 登录的账号：user_info 的邮箱，其次名字；不知道时为 ""。
    pub account: String,
    pub plan: AccountPlan,
    /// 快照里团队 provider 的团队项目（计划清单的顺序）。
    pub teams: Vec<CatalogConnection>,
    /// 计划对应的 provider：模型目录里它的模型是这个家族的。
    pub provider: String,
    /// 这个 provider 列出的全部内置模型，关着的也在（家族页的模型行，两个界面同一份）。
    pub models: Vec<ListedModel>,
}

/// user_info 里给人看的账号：zai 存后端原始对象（`email`、`name`），bigmodel 存 App 的 profile（`rawProfile` 里有
/// 原始对象，外面是 `displayName`、`username`）。
fn account_label(credentials: &Credentials, family: Family) -> Option<String> {
    let raw = credentials.present(&format!("oauth:{}:user_info", family.as_str()))?;
    let parsed: Value = serde_json::from_str(raw.expose()).ok()?;
    let inner = &parsed["rawProfile"];
    [
        &parsed["email"],
        &inner["email"],
        &parsed["name"],
        &inner["name"],
        &parsed["displayName"],
        &parsed["username"],
    ]
    .into_iter()
    .filter_map(Value::as_str)
    .map(str::trim)
    .find(|text| !text.is_empty())
    .map(str::to_owned)
}

impl Platform {
    /// 每个内置家族的账号状态（FAMILIES 的顺序）。凭据读不出时都算没有登录。
    pub async fn family_accounts(&self) -> Vec<FamilyAccount> {
        let view = self.usage_view().await;
        let selection = self.default_selection().await;
        let credentials = self.credentials.read().await.ok();
        let current = self.registry.current().await;
        FAMILIES
            .into_iter()
            .map(|family| {
                let plans = view.families.iter().find(|p| p.family == family);
                let individual = plans
                    .and_then(|p| p.individual.as_ref())
                    .map(|p| p.provider.clone())
                    .unwrap_or_default();
                let (team, teams) = plans
                    .and_then(|p| p.team.as_ref())
                    .map(|t| (t.provider.clone(), t.teams.clone()))
                    .unwrap_or_default();
                let start = format!("account:{}-start-plan", family.as_str());
                let signed_in = credentials.as_ref().is_some_and(|c| {
                    let login = Login::read(c, family);
                    (login.identity.is_some() && login.token.is_some())
                        || (!individual.is_empty() && account::manual_authorized(c, &individual))
                });
                let account = credentials
                    .as_ref()
                    .and_then(|c| account_label(c, family))
                    .unwrap_or_default();
                let (plan, provider) = match &selection {
                    Some(s) if !team.is_empty() && s.provider == team => {
                        let at = s
                            .connection
                            .as_deref()
                            .and_then(|c| teams.iter().position(|t| t.id == *c))
                            .unwrap_or(0);
                        (AccountPlan::Team(at), team)
                    }
                    Some(s) if s.provider == start => (AccountPlan::Start, start),
                    _ => (AccountPlan::Individual, individual),
                };
                let models = current
                    .snapshot
                    .listed
                    .iter()
                    .filter(|m| m.provider == provider)
                    .cloned()
                    .collect();
                FamilyAccount {
                    family,
                    signed_in,
                    account,
                    plan,
                    teams,
                    provider,
                    models,
                }
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "status_tests.rs"]
mod tests;
