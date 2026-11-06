//! `/usage`（docs/puffin-tui.md §4.2「用量（/usage）」；docs/specs/rust-cli/tui-kit.md §4.4 K5）：计划与额度的数据、host 的
//! 回答（`Seen`）、卡的条目，与卡、详情页、本地日历的画法。数据是纯的 serde 结构，从 Puffin 的投影
//! （Puffin 投影的 host/projection/quota.rs 与 lists.rs 的 `Family`）搬来。`Seen` 与 `Entry` 移植自 Puffin 的
//! shell/usage/{mod,data}.rs。
use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

pub mod calendar;
pub mod page;
pub mod view;

#[cfg(test)]
mod tests;

/// 本地时间与 UTC 差几分钟（按时刻，夏令时会变）：壳注入，日历按它算。
pub type UtcOffset = Arc<dyn Fn(f64) -> i32 + Send + Sync>;

/// 内置的两个账号家族。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Family {
    Zai,
    Bigmodel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UsageRange {
    #[serde(rename = "today")]
    Today,
    #[serde(rename = "7d")]
    Week,
    #[serde(rename = "30d")]
    Month,
}

/// 当前模型的 provider。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UsageModel {
    pub provider_id: String,
    /// 内置账号的 provider 之一。
    pub official: bool,
    pub name: String,
    /// 密钥管理页的主机；没有时为 ""。
    pub site: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsagePlan {
    /// 个人计划与 Start Plan 是 provider id；团队是 `team:family:productId:organizationId:projectId`。
    pub id: String,
    pub family: Family,
    pub kind: PlanKind,
    /// 团队的组织名；其余与不知道时为 ""。
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PlanKind {
    Individual,
    Team,
    Start,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PoolKind {
    FiveHour,
    Weekly,
    Tools,
    Mcp,
    Today,
    Once,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsagePool {
    pub pool: PoolKind,
    /// 剩余的百分比，0–100，不取整。
    pub left: f64,
    /// Tool calls 剩下的次数；其余为 None。
    #[serde(default)]
    pub count: Option<u64>,
    /// 下一次重置（毫秒）；没有时为 None。
    #[serde(default)]
    pub reset_at: Option<u64>,
    #[serde(default)]
    pub parts: Vec<PoolPart>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PoolPart {
    pub code: String,
    pub name: String,
    pub used: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum QuotaState {
    #[default]
    Ok,
    NoPlan,
    TeamEnded,
    NotOnTeam,
    Expired,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PlanQuota {
    /// 快照的时刻（毫秒）。
    pub at: u64,
    pub state: QuotaState,
    /// 套餐级别（`Pro`）；没有时为 ""。
    pub level: String,
    pub since: Option<u64>,
    pub renews: Option<u64>,
    pub ends: Option<u64>,
    pub pools: Vec<UsagePool>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Granularity {
    #[default]
    Hour,
    Day,
}

/// 一个计划的历史：各序列按 `axis` 逐点。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PlanDetail {
    pub at: u64,
    pub granularity: Granularity,
    /// `YYYY-MM-DD` 或 `HH:00:00`。
    pub axis: Vec<String>,
    pub cached: Vec<u64>,
    pub input: Vec<u64>,
    pub output: Vec<u64>,
    pub total: DetailTotal,
    /// 按多到少。
    pub models: Vec<NamedCount>,
    pub tools: Vec<NamedCount>,
    pub activity: Activity,
    /// 峰值 decode tokens/s。
    pub health: Health,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DetailTotal {
    pub tokens: u64,
    pub cache_rate: Option<f64>,
    pub credits: f64,
}

/// 模型的 token 或工具的调用次数。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct NamedCount {
    pub name: String,
    #[serde(alias = "tokens", alias = "calls")]
    pub count: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Activity {
    pub total: u64,
    pub best_day: u64,
    pub streak: u64,
    /// 第一周周日的日期；没有活动时为 ""。
    pub start: String,
    /// 逐周逐天（周日起，每周 7 个）的级别 0–4；没有那一天时为 -1。
    pub levels: Vec<Vec<i8>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Health {
    pub axis: Vec<String>,
    pub pro_max: Vec<f64>,
    pub lite: Vec<f64>,
}

/// host 的一个回答：现在的值、之前的值（换页时淡出）、到达的 tick、失败的原因。
#[derive(Clone, Debug, PartialEq)]
pub struct Seen<A> {
    pub value: Option<A>,
    pub before: Option<A>,
    pub arrived: i64,
    pub failed: Option<String>,
}

impl<A> Default for Seen<A> {
    fn default() -> Seen<A> {
        Seen {
            value: None,
            before: None,
            arrived: 0,
            failed: None,
        }
    }
}

impl<A> Seen<A> {
    /// 到了新的值：之前的值留作淡入的比较（UsageData.scala 第 54 行）。
    pub fn got(self, value: A, tick: i64) -> Seen<A> {
        Seen {
            before: self.value,
            value: Some(value),
            arrived: tick,
            failed: None,
        }
    }

    /// 问失败了：值留着，记下原因（第 55 行）。
    pub fn missed(self, why: String) -> Seen<A> {
        Seen {
            failed: Some(why),
            ..self
        }
    }
}

/// 卡的一个条目：ZCode 不跑的当前模型，或一个计划（Scala 的 `Either[UsageModel, UsagePlan]`）。
#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    Model(UsageModel),
    Plan(UsagePlan),
}

/// 卡与页要的读到的：计划、读计划失败的原因、各计划的额度与历史（按计划 id 与范围的名字）。
#[derive(Clone, Copy, Debug)]
pub struct Usage<'a> {
    pub plans: &'a [UsagePlan],
    pub plans_failed: Option<&'a str>,
    pub quotas: &'a BTreeMap<String, Seen<PlanQuota>>,
    pub details: &'a BTreeMap<(String, String), Seen<PlanDetail>>,
}

/// 详情页的范围：host 的名字与标签（UsageView.scala 第 13 行 `Ranges`）。
pub const RANGES: [(UsageRange, &str, &str); 3] = [
    (UsageRange::Today, "today", "Today"),
    (UsageRange::Week, "7d", "7 days"),
    (UsageRange::Month, "30d", "30 days"),
];

/// 页打开时的范围（7 天）。
pub const DEFAULT_RANGE: usize = 1;

/// 第 `i` 个范围（越界时取最后一个）。
pub fn range(i: usize) -> (UsageRange, &'static str, &'static str) {
    RANGES[i.min(RANGES.len() - 1)]
}

/// 范围的 host 名字（`today`、`7d`、`30d`）。
pub fn range_name(r: UsageRange) -> &'static str {
    RANGES
        .iter()
        .find(|(x, _, _)| *x == r)
        .map_or("7d", |(_, n, _)| n)
}
