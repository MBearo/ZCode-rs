// 语义复制自 packages/services/src/usage-stats/providers/bigmodelUsageQuotaProvider.ts 的 `getSnapshotForRequest`
// （Coding Plan 分支）与 `fetchQuota`、bigmodelSubscriptionProvider.ts 的 `fetchBigModelSubscriptionSummary`、
// bigmodelUsageQuotaMapper.ts 的 `normalizeLimits`、`pickCurrentSubscriptionFromList`（staging 420d27c977）。
//! 一个计划的额度（docs/specs/rust-cli/platform.md §11.3）：订阅摘要 → quota/limit ‖ MCP 额度，前后核对账号。

use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::model::ModelConnection;
use serde_json::Value;
use url::Url;

use super::{Context, PlanTarget, mcp, monitor_headers, number, set_param, successful, text, time};
use crate::personal::current_subscription;
use crate::team::{Availability, Reason, team_entitlement};
use crate::token::http::{Auth, Http, ORGANIZATION_HEADER, PROJECT_HEADER, Reply};
use rust_cli_base::account::Family;

/// 一个计划的额度快照（App `UsageEntitlementSnapshot` 里 Puffin 读的部分）。
#[derive(Debug, Clone, PartialEq)]
pub struct QuotaSnapshot {
    /// 快照的时刻（注入的时钟，App `generatedAt`）。
    pub at_ms: u64,
    pub plan: PlanStatus,
    /// quota/limit 的 `level` 与 `limits`；没有凭据、请求或信封失败时没有。
    pub quota: Option<Quota>,
    /// ZCode MCP 的额度（§11.4）。
    pub mcp: Option<QuotaLimit>,
    /// 下一次续费（自动续费的个人套餐）。
    pub renews_ms: Option<u64>,
    /// 到期（不自动续费的个人套餐，或团队的 `subscribeEndTime`）。
    pub ends_ms: Option<u64>,
}

/// 订阅的结论（App 的 `unavailableReason` 与 `teamPlanUnavailableReason`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanStatus {
    Available,
    NoPlan,
    /// 团队套餐已过期。
    TeamExpired,
    /// 不在团队套餐的成员里。
    TeamUnassigned,
    /// 个人计划没有凭据，订阅不明（App `not_configured`）。
    NotConfigured,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Quota {
    pub level: Option<String>,
    pub limits: Vec<QuotaLimit>,
}

/// quota/limit 的一项（App `UsageQuotaLimit`）：数值字段只认数字。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QuotaLimit {
    pub kind: String,
    pub unit: Option<f64>,
    pub number: Option<f64>,
    pub remaining: Option<f64>,
    /// 用掉的占比，0–100。
    pub percentage: Option<f64>,
    pub next_reset_ms: Option<f64>,
    pub details: Vec<UsageDetail>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageDetail {
    pub code: String,
    pub name: Option<String>,
    pub usage: f64,
}

/// 订阅查询的结论与摘要。
enum Subscription {
    Available {
        renews_ms: Option<u64>,
        ends_ms: Option<u64>,
    },
    Unavailable(PlanStatus),
    Unknown,
}

const REFRESH_FAILED: &str = "Coding Plan entitlement refresh failed";
const ACCOUNT_CHANGED: &str = "the account changed during the request";

pub(super) async fn fetch(cx: &Context<'_>, target: &PlanTarget) -> Result<QuotaSnapshot, String> {
    let at_ms = cx.clock.now_ms();
    let before = cx.login(target.family).await?;
    let quota_url = cx.quota_url(target.family)?;
    let origin = quota_url.origin().ascii_serialization();
    // 团队权益不以凭据为前提：换不到项目令牌时也保留订阅的结论（App 相同）。
    let local_offset = |at: u64| cx.clock.utc_offset_minutes(at);
    let team = match &target.team {
        Some(team) => Some(match &before.token {
            Some(token) => {
                team_subscription(
                    &cx.http,
                    (&origin, target.family),
                    team,
                    token,
                    &local_offset,
                )
                .await
            }
            None => Subscription::Unknown,
        }),
        None => None,
    };
    let credential = match cx.credential(target).await {
        Ok(credential) => Some(credential),
        Err(error) => {
            tracing::warn!(provider = %target.provider, %error, "cannot read the plan usage credential");
            None
        }
    };
    let subscription = match (team, &credential) {
        (Some(team), _) => team,
        (None, Some(credential)) => {
            personal_subscription(&cx.http, &origin, credential, &local_offset).await
        }
        (None, None) => Subscription::Unknown,
    };
    // 订阅不明不能当作空快照（App：已有查询身份时走失败路径）；没有凭据的个人计划是“没有配置”。
    if matches!(subscription, Subscription::Unknown)
        && (credential.is_some() || target.team.is_some())
    {
        return Err(REFRESH_FAILED.to_owned());
    }
    let (quota, mcp) = match &credential {
        Some(credential) => {
            tokio::join!(
                fetch_quota(&cx.http, quota_url, credential, target.team.as_ref()),
                mcp::fetch(cx, target, &before),
            )
        }
        None => (None, None),
    };
    if cx.login(target.family).await? != before {
        return Err(ACCOUNT_CHANGED.to_owned());
    }
    let (plan, renews_ms, ends_ms) = match subscription {
        Subscription::Available { renews_ms, ends_ms } => {
            (PlanStatus::Available, renews_ms, ends_ms)
        }
        Subscription::Unavailable(status) => (status, None, None),
        Subscription::Unknown => (PlanStatus::NotConfigured, None, None),
    };
    Ok(QuotaSnapshot {
        at_ms,
        plan,
        quota,
        mcp,
        renews_ms,
        ends_ms,
    })
}

/// 个人订阅（App `fetchPersonalCodingPlanEntitlement` + `pickCurrentSubscriptionFromList`）：任何请求失败都是 unknown。
async fn personal_subscription(
    http: &Http,
    origin: &str,
    credential: &RedactedString,
    local_offset: &(dyn Fn(u64) -> i32 + Sync),
) -> Subscription {
    let url = format!("{origin}/api/biz/subscription/list");
    let Ok(Reply::Json(payload)) = http.get(url, Auth::Raw(credential.expose())).await else {
        return Subscription::Unknown;
    };
    match current_subscription(&payload) {
        (Availability::Available, Some(item)) => {
            // 没有 productId 的有效项算不明（App `pickCurrentSubscriptionFromList` 给 null）。
            if text(&item["productId"]).is_none() {
                return Subscription::Unknown;
            }
            let (renews_ms, ends_ms) = personal_dates(item, local_offset);
            Subscription::Available { renews_ms, ends_ms }
        }
        (Availability::Unavailable(_), _) => Subscription::Unavailable(PlanStatus::NoPlan),
        _ => Subscription::Unknown,
    }
}

/// 续费与到期（App `pickCurrentSubscriptionRenewTime` / `pickCurrentSubscriptionExpireTime`）：自动续费时
/// `nextRenewTime` 是下一次扣费；不自动续费时它才是到期，否则到期取 `valid` 里最后一个日期。
pub(super) fn personal_dates(
    item: &Value,
    local_offset: &(dyn Fn(u64) -> i32 + Sync),
) -> (Option<u64>, Option<u64>) {
    let auto_renew =
        item["autoRenew"] == Value::Bool(true) || number(&item["autoRenew"]) == Some(1.0);
    let next = text(&item["nextRenewTime"]).and_then(|t| time::parse_time(t, local_offset));
    if auto_renew {
        let ends = item["valid"]
            .as_str()
            .and_then(last_date)
            .and_then(|t| time::parse_time(t, local_offset));
        return (next, ends);
    }
    let ends = next.or_else(|| {
        item["valid"]
            .as_str()
            .and_then(last_date)
            .and_then(|t| time::parse_time(t, local_offset))
    });
    (None, ends)
}

/// `valid.match(/\d{4}-\d{2}-\d{2}(?:[ T]\d{2}:\d{2}:\d{2})?/g)` 的最后一个。
fn last_date(valid: &str) -> Option<&str> {
    let bytes = valid.as_bytes();
    let digits = |at: usize, n: usize| {
        bytes
            .get(at..at + n)
            .is_some_and(|run| run.iter().all(u8::is_ascii_digit))
    };
    let mut found = None;
    let mut at = 0;
    while at < bytes.len() {
        let date = digits(at, 4)
            && bytes.get(at + 4) == Some(&b'-')
            && digits(at + 5, 2)
            && bytes.get(at + 7) == Some(&b'-')
            && digits(at + 8, 2);
        if !date {
            at += 1;
            continue;
        }
        let mut end = at + 10;
        if matches!(bytes.get(end), Some(b' ' | b'T'))
            && digits(end + 1, 2)
            && bytes.get(end + 3) == Some(&b':')
            && digits(end + 4, 2)
            && bytes.get(end + 6) == Some(&b':')
            && digits(end + 7, 2)
        {
            end += 9;
        }
        found = valid.get(at..end);
        at = end;
    }
    found
}

/// 团队订阅（App `fetchTeamCodingPlanEntitlement`，判定表同 §9.2）：登录 token（zai 用 Bearer、bigmodel 裸）与两个团队头；
/// 任何请求失败都是 unknown。
async fn team_subscription(
    http: &Http,
    (origin, family): (&str, Family),
    team: &ModelConnection,
    token: &RedactedString,
    local_offset: &(dyn Fn(u64) -> i32 + Sync),
) -> Subscription {
    let auth = match family {
        Family::Zai => Auth::Bearer(token.expose()),
        Family::Bigmodel => Auth::Raw(token.expose()),
    };
    let extra = [
        (ORGANIZATION_HEADER, team.organization_id.as_str()),
        (PROJECT_HEADER, team.project_id.as_str()),
    ];
    let url = format!("{origin}/api/biz/team/subscribe/product/querySubscribeDetail");
    let Ok(Reply::Json(payload)) = http.get_with(url, auth, &extra).await else {
        return Subscription::Unknown;
    };
    match team_entitlement(&payload) {
        Availability::Available => {
            // 团队的到期时刻原样交给 `Date.parse`（App 不经 `parseBigModelLocalDateTime`）：解析规则相同。
            let ends_ms = text(&payload["data"]["subscribeEndTime"])
                .and_then(|t| time::parse_time(t, local_offset));
            Subscription::Available {
                renews_ms: None,
                ends_ms,
            }
        }
        Availability::Unavailable(Reason::Expired) => {
            Subscription::Unavailable(PlanStatus::TeamExpired)
        }
        Availability::Unavailable(Reason::Unassigned) => {
            Subscription::Unavailable(PlanStatus::TeamUnassigned)
        }
        Availability::Unavailable(_) => Subscription::Unavailable(PlanStatus::NoPlan),
        Availability::Unknown => Subscription::Unknown,
    }
}

/// quota/limit（App `fetchQuota`）：失败与信封失败都没有（App `.catch(() => null)`）。
async fn fetch_quota(
    http: &Http,
    mut url: Url,
    credential: &RedactedString,
    team: Option<&ModelConnection>,
) -> Option<Quota> {
    // 团队按 `type=2` 路由到团队套餐（App `buildQuotaLimitUrl`）。
    if team.is_some() {
        set_param(&mut url, "type", "2");
    }
    let reply = http
        .get_headers(url.to_string(), monitor_headers(credential, team))
        .await;
    let payload = match reply {
        Ok(Reply::Json(payload)) => payload,
        Ok(Reply::NotFound) | Err(_) => {
            tracing::warn!("the plan quota request failed");
            return None;
        }
    };
    if !successful(&payload) {
        return None;
    }
    let data = payload.get("data").filter(|data| !data.is_null())?;
    Some(Quota {
        level: text(&data["level"]).map(str::to_owned),
        limits: normalize_limits(&data["limits"]),
    })
}

/// App `normalizeLimits`。
pub(super) fn normalize_limits(limits: &Value) -> Vec<QuotaLimit> {
    let Some(limits) = limits.as_array() else {
        return Vec::new();
    };
    limits
        .iter()
        .filter_map(|limit| {
            let kind = limit["type"].as_str().filter(|kind| !kind.is_empty())?;
            Some(QuotaLimit {
                kind: kind.to_owned(),
                unit: number(&limit["unit"]),
                number: number(&limit["number"]),
                remaining: number(&limit["remaining"]),
                percentage: number(&limit["percentage"]),
                next_reset_ms: number(&limit["nextResetTime"]),
                details: limit["usageDetails"]
                    .as_array()
                    .map(|details| {
                        details
                            .iter()
                            .filter_map(|detail| {
                                Some(UsageDetail {
                                    code: detail["modelCode"].as_str()?.to_owned(),
                                    name: text(&detail["displayName"]).map(str::to_owned),
                                    usage: number(&detail["usage"]).unwrap_or(0.0),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect()
}

#[cfg(test)]
#[path = "quota_tests.rs"]
mod tests;
