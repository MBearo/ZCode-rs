// 语义复制自 packages/tui/host/src/usageHost.ts（feat/tui a024959dca）：`sources`、`sourceOf`、`fresh`、`quotaOf`、
// `codingPools`、`detailOf`。
//! `/usage` 的端口（docs/specs/rust-cli/platform.md §11.1、§11.2）：界面的 `PlanUsage`（`rust_cli_tui::services`）。选计划，
//! 问 platform 的套餐用量，整理成 kit 的形状。每个问题一分钟内的结果直接给出，同一个问题同时只有一个请求；失败不留（App 的
//! freshness window，docs/usage-remaining-refresh-policy.md）。没有轮询：界面问时才问。迁自已删除的 Puffin host
//! 的 `usage.rs`：以前界面的端口经它的问答枚举转一道，现在直接实现。
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rust_cli_base::sync::LockUnpoisoned;

use futures_util::future::{BoxFuture, FutureExt, Shared as SharedFuture};
use rust_cli_base::account::Family as AccountFamily;
use rust_cli_kernel_api::clock::Clock;
use rust_cli_platform::Platform;
use rust_cli_platform::usage::{
    HistoryRange, HistorySnapshot, OFFICIAL_PROVIDERS, PlanStatus, PlanTarget, QuotaLimit,
    QuotaSnapshot, UsageView,
};
use rust_cli_tui_kit::app::widgets::usage::{
    Activity, DetailTotal, Family, Granularity, Health, NamedCount, PlanDetail, PlanKind,
    PlanQuota, PoolKind, PoolPart, QuotaState, UsageModel, UsagePlan, UsagePool, UsageRange,
};

/// 一分钟内的结果不再问（usageHost `USAGE_FRESH_MS`）。
const USAGE_FRESH_MS: u64 = 60_000;

/// 套餐用量的来源：生产是 [`Platform`]（platform.md §11），测试用替身。
pub(crate) trait PlanSource: Send + Sync {
    fn view(&self) -> BoxFuture<'_, UsageView>;
    fn quota(&self, target: PlanTarget) -> BoxFuture<'_, Result<QuotaSnapshot, String>>;
    fn history(
        &self,
        target: PlanTarget,
        range: HistoryRange,
    ) -> BoxFuture<'_, Result<HistorySnapshot, String>>;
}

impl PlanSource for Platform {
    fn view(&self) -> BoxFuture<'_, UsageView> {
        self.usage_view().boxed()
    }

    fn quota(&self, target: PlanTarget) -> BoxFuture<'_, Result<QuotaSnapshot, String>> {
        async move { self.plan_quota(&target).await }.boxed()
    }

    fn history(
        &self,
        target: PlanTarget,
        range: HistoryRange,
    ) -> BoxFuture<'_, Result<HistorySnapshot, String>> {
        async move { self.plan_history(&target, range).await }.boxed()
    }
}

type Flight<T> = SharedFuture<BoxFuture<'static, Result<T, String>>>;

/// 一种回答的一分钟缓存与在途的请求。
struct Kept<T> {
    /// 问题 → (开始问的时刻, 结果)。
    answers: HashMap<String, (u64, T)>,
    flying: HashMap<String, Flight<T>>,
}

impl<T> Default for Kept<T> {
    fn default() -> Self {
        Self {
            answers: HashMap::new(),
            flying: HashMap::new(),
        }
    }
}

/// 端口：额度与历史各一份缓存（键里带计划与范围，同以前的一张表）。克隆的端口共用缓存。
#[derive(Clone)]
pub(crate) struct UsagePort {
    source: Arc<dyn PlanSource>,
    clock: Arc<dyn Clock>,
    quotas: Arc<Mutex<Kept<PlanQuota>>>,
    details: Arc<Mutex<Kept<PlanDetail>>>,
}

impl UsagePort {
    pub(crate) fn new(source: Arc<dyn PlanSource>, clock: Arc<dyn Clock>) -> Self {
        Self {
            source,
            clock,
            quotas: Arc::default(),
            details: Arc::default(),
        }
    }

    /// 按 id 找计划（usageHost `sourceOf`）：把 id 当作当前模型的 provider 来列，个人计划没有授权时也在里面。
    async fn target(&self, plan: &str) -> Result<PlanTarget, String> {
        sources(&self.source.view().await, plan)
            .1
            .into_iter()
            .find(|source| source.plan.id == plan)
            .map(|source| source.target)
            .ok_or_else(|| format!("unknown plan: {plan}"))
    }
}

/// 一分钟内读过就给那一次；正在读就等它；失败不留（usageHost `fresh`）。
fn fresh<T: Clone + Send + Sync + 'static>(
    kept: &Arc<Mutex<Kept<T>>>,
    started: u64,
    key: String,
    read: impl FnOnce() -> BoxFuture<'static, Result<T, String>>,
) -> BoxFuture<'static, Result<T, String>> {
    // 锁中毒时数据仍然一致：每次修改都是单步的插入或删除。
    let mut held = kept.lock_unpoisoned();
    if let Some((at, answer)) = held.answers.get(&key)
        && started.saturating_sub(*at) < USAGE_FRESH_MS
    {
        let answer = answer.clone();
        return async move { Ok(answer) }.boxed();
    }
    if let Some(flight) = held.flying.get(&key) {
        return flight.clone().boxed();
    }
    let owner = kept.clone();
    let reading = read();
    let flight = {
        let key = key.clone();
        async move {
            let result = reading.await;
            let mut held = owner.lock_unpoisoned();
            held.flying.remove(&key);
            if let Ok(answer) = &result {
                held.answers.insert(key, (started, answer.clone()));
            }
            result
        }
        .boxed()
        .shared()
    };
    held.flying.insert(key, flight.clone());
    flight.boxed()
}

impl rust_cli_tui::services::PlanUsage for UsagePort {
    fn plans(
        &self,
        provider: String,
    ) -> BoxFuture<'static, Result<(UsageModel, Vec<UsagePlan>), String>> {
        let port = self.clone();
        async move {
            let (model, sources) = sources(&port.source.view().await, &provider);
            Ok((
                model,
                sources.into_iter().map(|source| source.plan).collect(),
            ))
        }
        .boxed()
    }

    fn quota(&self, plan: String) -> BoxFuture<'static, Result<PlanQuota, String>> {
        let port = self.clone();
        async move {
            let target = port.target(&plan).await?;
            let source = port.source.clone();
            let started = port.clock.now_ms();
            fresh(&port.quotas, started, format!("quota:{plan}"), move || {
                async move { Ok(quota_of(&source.quota(target).await?)) }.boxed()
            })
            .await
        }
        .boxed()
    }

    fn detail(
        &self,
        plan: String,
        range: UsageRange,
    ) -> BoxFuture<'static, Result<PlanDetail, String>> {
        let port = self.clone();
        async move {
            let target = port.target(&plan).await?;
            let source = port.source.clone();
            let started = port.clock.now_ms();
            let key = format!("detail:{plan}:{}", range_key(range));
            fresh(&port.details, started, key, move || {
                async move {
                    let history = source.history(target, history_range(range)).await?;
                    Ok(detail_of(&history))
                }
                .boxed()
            })
            .await
        }
        .boxed()
    }
}

/// 一个计划与问它用的目标。
#[derive(Debug, Clone, PartialEq)]
struct Source {
    plan: UsagePlan,
    target: PlanTarget,
}

fn family_of(family: AccountFamily) -> Family {
    match family {
        AccountFamily::Zai => Family::Zai,
        AccountFamily::Bigmodel => Family::Bigmodel,
    }
}

/// 计划清单（usageHost `sources`，platform.md §11.2）：每个 family 依次放个人计划（已授权，或是当前模型的 provider
/// 时）与每个团队项目；当前模型的计划在最前。
fn sources(view: &UsageView, provider: &str) -> (UsageModel, Vec<Source>) {
    let mut all = Vec::new();
    for plans in &view.families {
        let family = family_of(plans.family);
        if let Some(individual) = &plans.individual
            && (individual.entitled || individual.provider == provider)
        {
            all.push(Source {
                plan: UsagePlan {
                    id: individual.provider.clone(),
                    family,
                    kind: PlanKind::Individual,
                    name: String::new(),
                },
                target: PlanTarget {
                    provider: individual.provider.clone(),
                    family: plans.family,
                    team: None,
                },
            });
        }
        for team in plans.team.iter().flat_map(|team| {
            team.teams
                .iter()
                .map(move |connection| (&team.provider, connection))
        }) {
            let (team_provider, connection) = team;
            let id = &connection.id;
            all.push(Source {
                plan: UsagePlan {
                    id: format!(
                        "team:{}:{}:{}:{}",
                        plans.family.as_str(),
                        id.product_id,
                        id.organization_id,
                        id.project_id
                    ),
                    family,
                    kind: PlanKind::Team,
                    name: connection.name.clone(),
                },
                target: PlanTarget {
                    provider: team_provider.clone(),
                    family: plans.family,
                    team: Some(id.clone()),
                },
            });
        }
    }
    // 个人 provider 是它的个人计划；团队 provider 是它的第一个团队项目。
    if let Some(current) = all
        .iter()
        .position(|source| source.target.provider == provider)
    {
        let first = all.remove(current);
        all.insert(0, first);
    }
    let facts = view
        .providers
        .iter()
        .find(|facts| facts.provider == provider);
    let name = facts
        .map(|facts| facts.name.trim())
        .filter(|name| !name.is_empty())
        .unwrap_or(provider);
    let site = facts
        .and_then(|facts| facts.key_url.as_deref())
        .and_then(|url| url::Url::parse(url).ok())
        .map(|url| host_of(&url))
        .unwrap_or_default();
    let model = UsageModel {
        provider_id: provider.to_owned(),
        official: OFFICIAL_PROVIDERS.contains(&provider),
        name: name.to_owned(),
        site,
    };
    (model, all)
}

/// JS `new URL(url).host`：主机与非缺省端口。
fn host_of(url: &url::Url) -> String {
    match (url.host_str(), url.port()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_owned(),
        (None, _) => String::new(),
    }
}

fn range_key(range: UsageRange) -> &'static str {
    match range {
        UsageRange::Today => "today",
        UsageRange::Week => "7d",
        UsageRange::Month => "30d",
    }
}

fn history_range(range: UsageRange) -> HistoryRange {
    match range {
        UsageRange::Today => HistoryRange::Today,
        UsageRange::Week => HistoryRange::Week,
        UsageRange::Month => HistoryRange::Month,
    }
}

/// 非负整数（投影的计数都是整数；JS 的数到这里取整）。
fn whole(value: f64) -> u64 {
    if value.is_finite() && value > 0.0 {
        // 用量的计数远小于 2^53，取整后转换不会截断。
        value.round() as u64
    } else {
        0
    }
}

/// 毫秒时刻（非负的才有）。
fn instant(value: Option<f64>) -> Option<u64> {
    value.filter(|v| v.is_finite() && *v >= 0.0).map(whole)
}

/// zai 的团队计划用 CREDIT_LIMIT，与 TOKENS_LIMIT 同义（usageHost `TOKEN_TYPES`）。
const TOKEN_TYPES: [&str; 2] = ["TOKENS_LIMIT", "CREDIT_LIMIT"];

/// Coding Plan 的 percentage 是用掉的占比（0–100）。
fn left_of_used(limit: &QuotaLimit) -> f64 {
    (100.0 - limit.percentage.unwrap_or(0.0)).clamp(0.0, 100.0)
}

fn pool_of(limit: &QuotaLimit, pool: PoolKind) -> UsagePool {
    UsagePool {
        pool,
        left: left_of_used(limit),
        count: (pool == PoolKind::Tools)
            .then_some(limit.remaining)
            .flatten()
            .map(whole),
        reset_at: instant(limit.next_reset_ms),
        parts: limit
            .details
            .iter()
            .map(|detail| PoolPart {
                code: detail.code.clone(),
                name: detail.name.clone().unwrap_or_default(),
                used: whole(detail.usage),
            })
            .collect(),
    }
}

/// usageHost `codingPools`。
fn coding_pools(snapshot: &QuotaSnapshot) -> Vec<UsagePool> {
    let limits = snapshot
        .quota
        .as_ref()
        .map(|quota| quota.limits.as_slice())
        .unwrap_or_default();
    let find = |types: &[&str], unit: f64, number: Option<f64>| {
        limits.iter().find(|limit| {
            types.contains(&limit.kind.as_str())
                && limit.unit == Some(unit)
                && number.is_none_or(|number| limit.number == Some(number))
        })
    };
    let mut pools = Vec::new();
    if let Some(limit) = find(&TOKEN_TYPES, 3.0, Some(5.0)) {
        pools.push(pool_of(limit, PoolKind::FiveHour));
    }
    if let Some(limit) = find(&TOKEN_TYPES, 6.0, None) {
        pools.push(pool_of(limit, PoolKind::Weekly));
    }
    if let Some(limit) = find(&["TIME_LIMIT"], 5.0, Some(1.0)) {
        pools.push(pool_of(limit, PoolKind::Tools));
    }
    if let Some(limit) = &snapshot.mcp {
        pools.push(pool_of(limit, PoolKind::Mcp));
    }
    pools
}

/// 首字母大写（usageHost `levelOf`）。
fn level_of(level: Option<&str>) -> String {
    let text = level.unwrap_or_default().trim();
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// usageHost `quotaOf`（Coding Plan；Rust 没有 Start Plan，D-U-1）。
fn quota_of(snapshot: &QuotaSnapshot) -> PlanQuota {
    let state = match snapshot.plan {
        PlanStatus::Available => QuotaState::Ok,
        PlanStatus::NoPlan => QuotaState::NoPlan,
        PlanStatus::TeamExpired => QuotaState::TeamEnded,
        PlanStatus::TeamUnassigned => QuotaState::NotOnTeam,
        PlanStatus::NotConfigured => QuotaState::Expired,
    };
    PlanQuota {
        at: snapshot.at_ms,
        state,
        level: level_of(
            snapshot
                .quota
                .as_ref()
                .and_then(|quota| quota.level.as_deref()),
        ),
        // Coding Plan 的订阅没有开始时刻。
        since: None,
        renews: snapshot.renews_ms,
        ends: snapshot.ends_ms,
        pools: if state == QuotaState::Ok {
            coding_pools(snapshot)
        } else {
            Vec::new()
        },
    }
}

/// 按多到少（同数时保持原来的顺序，JS 的稳定排序）。
fn ranked(items: impl Iterator<Item = (String, f64)>) -> Vec<NamedCount> {
    let mut items: Vec<(String, f64)> = items.filter(|(_, count)| *count > 0.0).collect();
    items.sort_by(|(_, left), (_, right)| right.total_cmp(left));
    items
        .into_iter()
        .map(|(name, count)| NamedCount {
            name,
            count: whole(count),
        })
        .collect()
}

/// usageHost `detailOf`。
fn detail_of(history: &HistorySnapshot) -> PlanDetail {
    let length = history.axis.len();
    let sum_at = |series: &dyn Fn(&rust_cli_platform::usage::ModelUsage) -> &[f64]| {
        (0..length)
            .map(|index| {
                whole(
                    history
                        .models
                        .iter()
                        .map(|model| series(model).get(index).copied().unwrap_or(0.0))
                        .sum(),
                )
            })
            .collect()
    };
    let activity = &history.activity;
    PlanDetail {
        at: history.at_ms,
        granularity: if history.hourly {
            Granularity::Hour
        } else {
            Granularity::Day
        },
        axis: history.axis.clone(),
        cached: sum_at(&|model| &model.cached),
        input: sum_at(&|model| &model.uncached),
        output: sum_at(&|model| &model.output),
        total: DetailTotal {
            tokens: whole(history.total_tokens),
            // 有时给 0–100：大于 1 时除以 100（同桌面端 formatCodingPlanRate）。
            cache_rate: history
                .cache_hit_rate
                .map(|rate| if rate > 1.0 { rate / 100.0 } else { rate }),
            credits: history.total_credits,
        },
        models: ranked(
            history
                .models
                .iter()
                .map(|model| (model.name.clone(), model.total_tokens)),
        ),
        tools: ranked(
            history
                .tools
                .iter()
                .map(|tool| (tool.name.clone(), tool.calls)),
        ),
        activity: Activity {
            total: whole(activity.total_tokens),
            best_day: whole(activity.peak_daily_tokens),
            streak: whole(activity.streak_days),
            start: activity.start.clone().unwrap_or_default(),
            levels: activity
                .weeks
                .iter()
                .map(|week| {
                    week.iter()
                        .map(|day| day.map_or(-1, |level| i8::try_from(level).unwrap_or(4)))
                        .collect()
                })
                .collect(),
        },
        health: Health {
            axis: history.health.axis.clone(),
            pro_max: history.health.pro_max.clone(),
            lite: history.health.lite.clone(),
        },
    }
}

#[cfg(test)]
#[path = "tui_usage_tests.rs"]
mod tests;
