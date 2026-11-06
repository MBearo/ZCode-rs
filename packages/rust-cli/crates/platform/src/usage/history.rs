// 语义复制自 packages/services/src/usage-stats/providers/bigmodelUsageQuotaProvider.ts 的
// `getCodingPlanUsageSnapshot`、`buildCreditUsageMonitorUrl`、`buildModelPerformanceMonitorUrl`、
// `readBestEffortBigModelMonitorData`，bigmodelUsageMonitorRange.ts 的 `resolveCodingPlanUsageTimeRange`，与
// bigmodelUsageMonitorMapper.ts 的 credit-usage 分支（staging 420d27c977）。
//! 一个计划的历史（docs/specs/rust-cli/platform.md §11.5）：activity、两份 usage-detail 与解码速度同时取。

use std::collections::{BTreeMap, HashMap};

use rust_cli_base::secret::RedactedString;
use serde_json::Value;
use url::Url;

use super::{
    Context, PlanTarget, envelope_message, explicitly_successful, finite, monitor_headers,
    replace_path_end, set_param, successful, text, time,
};
use crate::token::http::{Http, Reply};

/// 历史的范围（App `today` / `7d` / `30d`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HistoryRange {
    Today,
    Week,
    Month,
}

/// 一个计划在一个范围里的历史（App `CodingPlanUsageSnapshot` 里 Puffin 读的部分）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HistorySnapshot {
    pub at_ms: u64,
    /// 按小时（今天）还是按天。
    pub hourly: bool,
    /// 轴：按小时是 `HH:00:00`，按天是 `YYYY-MM-DD`。
    pub axis: Vec<String>,
    /// 模型（去掉计费拆分桶，按 `sortOrder`）。
    pub models: Vec<ModelUsage>,
    /// 合计 token（App `sumCreditModelTokens`）。
    pub total_tokens: f64,
    pub cache_hit_rate: Option<f64>,
    pub total_credits: f64,
    /// 工具（按 `sortOrder`）。
    pub tools: Vec<ToolUsage>,
    pub activity: ActivitySnapshot,
    pub health: HealthSnapshot,
}

/// 一个模型：三条序列与轴对齐。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelUsage {
    pub name: String,
    pub cached: Vec<f64>,
    pub uncached: Vec<f64>,
    pub output: Vec<f64>,
    pub total_tokens: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolUsage {
    pub name: String,
    pub calls: f64,
}

/// 一年的活动（App heatmap）：只有有数据的周，周日起；没有数据的天为空。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ActivitySnapshot {
    pub total_tokens: f64,
    pub peak_daily_tokens: f64,
    pub streak_days: f64,
    /// 第一周的周日（`YYYY-MM-DD`）。
    pub start: Option<String>,
    /// 每天的级别 0–4。
    pub weeks: Vec<[Option<u8>; 7]>,
}

/// 近 7 天的解码速度。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HealthSnapshot {
    pub axis: Vec<String>,
    pub pro_max: Vec<f64>,
    pub lite: Vec<f64>,
}

/// 范围的时刻与轴（App `resolveCodingPlanUsageTimeRange`）。
pub(super) struct Span {
    pub start: String,
    pub end: String,
    pub hourly: bool,
    pub axis: Vec<String>,
}

pub(super) fn span(range: HistoryRange, today: i64) -> Span {
    let end_key = time::date_of_day(today);
    let days = match range {
        HistoryRange::Today => {
            return Span {
                start: format!("{end_key} 00:00:00"),
                end: format!("{end_key} 23:59:59"),
                hourly: true,
                axis: (0..24).map(|hour| format!("{hour:02}:00:00")).collect(),
            };
        }
        HistoryRange::Week => 7,
        HistoryRange::Month => 30,
    };
    let first = today - (days - 1);
    Span {
        start: format!("{} 00:00:00", time::date_of_day(first)),
        end: format!("{end_key} 23:59:59"),
        hourly: false,
        axis: (first..=today).map(time::date_of_day).collect(),
    }
}

pub(super) async fn fetch(
    cx: &Context<'_>,
    target: &PlanTarget,
    range: HistoryRange,
) -> Result<HistorySnapshot, String> {
    let credential = match cx.credential(target).await {
        Ok(credential) => credential,
        Err(error) => {
            // 缺 key 时不回退别的凭据（App `resolveCodingPlanApiKeyError`）。
            tracing::warn!(provider = %target.provider, %error, "cannot read the plan usage credential");
            return Err(format!(
                "{}_coding_plan_api_key_required",
                target.family.as_str()
            ));
        }
    };
    let at_ms = cx.clock.now_ms();
    let today = time::today(cx.clock.as_ref());
    let span = span(range, today);
    let base = cx.quota_url(target.family)?;
    let kind = if target.team.is_some() { "3" } else { "1" };
    let credit = |endpoint: &str, (start, end): (&str, &str), usage: Option<&str>| {
        let mut url = base.clone();
        replace_path_end(
            &mut url,
            "/usage/quota/limit",
            &format!("/credit-usage/{endpoint}"),
        );
        set_param(&mut url, "type", kind);
        set_param(&mut url, "startTime", start);
        set_param(&mut url, "endTime", end);
        if let Some(usage) = usage {
            set_param(&mut url, "usageType", usage);
        }
        url
    };
    let year_start = format!("{} 00:00:00", time::date_of_day(today - 365));
    let week_start = format!("{} 00:00:00", time::date_of_day(today - 6));
    let activity_url = credit("activity", (&year_start, &span.end), None);
    let models_url = credit("usage-detail", (&span.start, &span.end), Some("MODEL"));
    let tools_url = credit("usage-detail", (&span.start, &span.end), Some("MCP"));
    let mut health_url = base.clone();
    replace_path_end(&mut health_url, "/quota/limit", "/model-performance-day");
    set_param(&mut health_url, "startTime", &week_start);
    set_param(&mut health_url, "endTime", &span.end);
    let team = target.team.as_ref();
    let read = |url: Url, what: &'static str| monitor(&cx.http, url, &credential, team, what);
    let (activity, models, tools, health) = tokio::join!(
        read(activity_url, "BigModel activity usage failed"),
        read(models_url, "BigModel model usage detail failed"),
        read(tools_url, "BigModel tool usage detail failed"),
        read(health_url, "BigModel model performance failed"),
    );
    let (activity, models, tools, health) = (activity?, models?, tools?, health?);
    Ok(snapshot(
        at_ms,
        &span,
        [&activity, &models, &tools, &health],
    ))
}

/// 一个 monitor 请求（App `readBestEffortBigModelMonitorData`）：传输失败为空（记 warn）；信封失败整体失败；信封成功
/// 但没有 `data` 时，有明确的成功信号为空，否则失败。
async fn monitor(
    http: &Http,
    url: Url,
    credential: &RedactedString,
    team: Option<&rust_cli_kernel_api::model::ModelConnection>,
    what: &'static str,
) -> Result<Value, String> {
    let payload = match http
        .get_headers(url.to_string(), monitor_headers(credential, team))
        .await
    {
        Ok(Reply::Json(payload)) => payload,
        Ok(Reply::NotFound) | Err(_) => {
            tracing::warn!("{what}; the section is left empty");
            return Ok(Value::Null);
        }
    };
    let failed = || {
        let message = envelope_message(&payload);
        if message.is_empty() {
            what.to_owned()
        } else {
            message
        }
    };
    if !successful(&payload) {
        return Err(failed());
    }
    match payload.get("data") {
        Some(data) if truthy(data) => Ok(data.clone()),
        _ if explicitly_successful(&payload) => Ok(Value::Null),
        _ => Err(failed()),
    }
}

/// JS 的真值（`if (payload.data)`）。
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// 四份数据 → 快照（App `buildCodingPlanUsageSnapshotFromMonitor` 的 credit-usage 分支）。
pub(super) fn snapshot(
    at_ms: u64,
    span: &Span,
    [activity, models, tools, health]: [&Value; 4],
) -> HistorySnapshot {
    let model_usage = &models["modelUsage"];
    let has_models = !model_usage.is_null();
    let summary = &models["summary"];
    HistorySnapshot {
        at_ms,
        hourly: span.hourly,
        axis: span.axis.clone(),
        models: if has_models {
            model_list(model_usage, &span.axis, span.hourly)
        } else {
            Vec::new()
        },
        total_tokens: if has_models {
            sum_tokens(model_usage)
        } else {
            0.0
        },
        cache_hit_rate: finite(&summary["cacheHitRate"]["value"]),
        total_credits: finite(&summary["totalCredits"]["value"]).unwrap_or(0.0),
        tools: if tools["mcpUsage"].is_null() {
            Vec::new()
        } else {
            tool_list(&tools["mcpUsage"])
        },
        activity: activity_of(activity),
        health: HealthSnapshot {
            axis: strings(if health["x_time"].is_null() {
                &health["xTime"]
            } else {
                &health["x_time"]
            }),
            pro_max: numbers(&health["proMaxDecodeSpeed"]),
            lite: numbers(&health["liteDecodeSpeed"]),
        },
    }
}

/// App `CREDIT_USAGE_BREAKDOWN_BUCKET_CODES` 与中文的桶名：计费组成，不是模型。
fn breakdown_bucket(item: &Value) -> bool {
    const CODES: [&str; 9] = [
        "cached_input",
        "cachedInput",
        "cache_input",
        "cacheInput",
        "uncached_input",
        "uncachedInput",
        "output",
        "output_tokens",
        "outputTokens",
    ];
    if text(&item["modelCode"]).is_some_and(|code| CODES.contains(&code)) {
        return true;
    }
    matches!(text(&item["modelName"]), Some("缓存" | "未缓存" | "输出"))
}

/// 一行的 token 序列（`totalTokensUsage`、`tokensUsage`、三条之和里第一条有正数的）。
fn token_series(item: &Value) -> Vec<f64> {
    let parts = sum_series(&[
        &item["cachedInputTokensUsage"],
        &item["uncachedInputTokensUsage"],
        &item["outputTokensUsage"],
    ]);
    first_positive(&[&item["totalTokensUsage"], &item["tokensUsage"]])
        .or_else(|| parts.filter(|series| series.iter().any(|v| *v > 0.0)))
        .unwrap_or_default()
}

/// App `normalizeCreditModelDataList`。
fn model_list(usage: &Value, axis: &[String], hourly: bool) -> Vec<ModelUsage> {
    let source = strings(&usage["xTime"]);
    let align = |values: &Value| align(&source, axis, &numbers(values), hourly);
    let mut rows: Vec<(f64, ModelUsage)> = array(&usage["modelDataList"])
        .iter()
        .filter(|item| text(&item["modelName"]).is_some() || text(&item["modelCode"]).is_some())
        .filter(|item| !breakdown_bucket(item))
        .enumerate()
        .map(|(index, item)| {
            let order = item["sortOrder"].as_f64().unwrap_or(index as f64);
            let name = text(&item["modelName"])
                .or_else(|| text(&item["modelCode"]))
                .unwrap_or("unknown")
                .to_owned();
            let total_tokens =
                finite(&item["totalTokens"]).unwrap_or_else(|| token_series(item).iter().sum());
            let model = ModelUsage {
                name,
                cached: align(&item["cachedInputTokensUsage"]),
                uncached: align(&item["uncachedInputTokensUsage"]),
                output: align(&item["outputTokensUsage"]),
                total_tokens,
            };
            (order, model)
        })
        .collect();
    rows.sort_by(|(left, _), (right, _)| left.total_cmp(right));
    rows.into_iter().map(|(_, model)| model).collect()
}

/// App `sumCreditModelTokens`：有真实模型行时只算模型行，否则算拆分桶。
fn sum_tokens(usage: &Value) -> f64 {
    let items = array(&usage["modelDataList"]);
    let real = items.iter().any(|item| !breakdown_bucket(item));
    items
        .iter()
        .filter(|item| !real || !breakdown_bucket(item))
        .map(|item| finite(&item["totalTokens"]).unwrap_or_else(|| token_series(item).iter().sum()))
        .sum()
}

/// App `normalizeCreditToolDataList` 的名字与次数。
fn tool_list(usage: &Value) -> Vec<ToolUsage> {
    let mut rows: Vec<(f64, ToolUsage)> = array(&usage["mcpDataList"])
        .iter()
        .filter(|item| {
            ["mcpCode", "mcpName", "toolCode", "toolName"]
                .iter()
                .any(|name| text(&item[*name]).is_some())
        })
        .enumerate()
        .map(|(index, item)| {
            let order = item["sortOrder"].as_f64().unwrap_or(index as f64);
            let code = text(&item["mcpCode"])
                .or_else(|| text(&item["toolCode"]))
                .or_else(|| text(&item["mcpName"]))
                .unwrap_or("unknown");
            let name = text(&item["mcpName"])
                .or_else(|| text(&item["toolName"]))
                .or_else(|| text(&item["mcpCode"]))
                .unwrap_or(code)
                .to_owned();
            let calls = finite(&item["totalUsageCount"]).unwrap_or_else(|| {
                first_positive(&[&item["mcpCallCount"], &item["usageCount"]])
                    .unwrap_or_default()
                    .iter()
                    .sum()
            });
            (order, ToolUsage { name, calls })
        })
        .collect();
    rows.sort_by(|(left, _), (right, _)| left.total_cmp(right));
    rows.into_iter().map(|(_, tool)| tool).collect()
}

/// App `buildCodingPlanActivitySnapshot` 与 heatmap。
fn activity_of(activity: &Value) -> ActivitySnapshot {
    let summary = &activity["summary"];
    let mut cells: Vec<(String, f64)> = array(&activity["series"])
        .iter()
        .filter_map(|item| {
            let date = item["date"].as_str().unwrap_or_default().trim();
            date_key(date).then(|| (date.to_owned(), finite(&item["totalTokens"]).unwrap_or(0.0)))
        })
        .collect();
    cells.sort_by(|(left, _), (right, _)| left.cmp(right));
    let max = cells.iter().map(|(_, tokens)| *tokens).fold(0.0, f64::max);
    // 同一天出现两次时后一个生效（JS `new Map`）。
    let by_date: HashMap<&str, f64> = cells.iter().map(|(d, t)| (d.as_str(), *t)).collect();
    let mut weeks: BTreeMap<i64, [Option<u8>; 7]> = BTreeMap::new();
    for (date, _) in &cells {
        let Some(day) = time::day_of_date(date) else {
            continue;
        };
        let start = day - time::weekday(day);
        weeks.entry(start).or_insert_with(|| {
            std::array::from_fn(|index| {
                let key = time::date_of_day(start + index as i64);
                by_date.get(key.as_str()).map(|tokens| level(*tokens, max))
            })
        });
    }
    let start = weeks
        .iter()
        .next()
        .filter(|(_, days)| days.iter().any(Option::is_some))
        .map(|(start, _)| time::date_of_day(*start));
    ActivitySnapshot {
        total_tokens: finite(&summary["totalTokens"]).unwrap_or(0.0),
        peak_daily_tokens: finite(&summary["peakDailyTokens"]).unwrap_or(0.0),
        streak_days: finite(&summary["currentStreakDays"]).unwrap_or(0.0),
        start,
        weeks: weeks.into_values().collect(),
    }
}

/// App `levelForTokens`。
fn level(value: f64, max: f64) -> u8 {
    if value <= 0.0 || max <= 0.0 {
        return 0;
    }
    // 1–4 之内，转换不会截断。
    (value / max * 4.0).ceil().clamp(1.0, 4.0) as u8
}

/// `/^\d{4}-\d{2}-\d{2}$/`。
fn date_key(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 10
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            4 | 7 => *byte == b'-',
            _ => byte.is_ascii_digit(),
        })
}

/// App `alignUsageSeries`。
pub(super) fn align(
    source: &[String],
    target: &[String],
    values: &[f64],
    hourly: bool,
) -> Vec<f64> {
    if source.is_empty() && values.len() == target.len() {
        return values.to_vec();
    }
    let mut by_time: HashMap<String, f64> = HashMap::new();
    for (index, at) in source.iter().enumerate() {
        by_time.insert(
            axis_key(at, hourly),
            values.get(index).copied().unwrap_or(0.0),
        );
    }
    target
        .iter()
        .enumerate()
        .map(|(index, at)| {
            by_time
                .get(&axis_key(at, hourly))
                .copied()
                .unwrap_or_else(|| {
                    if source.is_empty() {
                        values.get(index).copied().unwrap_or(0.0)
                    } else {
                        0.0
                    }
                })
        })
        .collect()
}

/// App `normalizeCodingPlanAxisValue`：按小时时找第一个 `HH:MM(:SS)`，其次找第一个 `YYYY-MM-DD`，都没有时原样。
pub(super) fn axis_key(value: &str, hourly: bool) -> String {
    let bytes = value.as_bytes();
    let digits = |at: usize, n: usize| {
        bytes
            .get(at..at + n)
            .is_some_and(|run| run.iter().all(u8::is_ascii_digit))
    };
    if hourly {
        for at in 0..bytes.len() {
            if digits(at, 2) && bytes.get(at + 2) == Some(&b':') && digits(at + 3, 2) {
                let seconds = if bytes.get(at + 5) == Some(&b':') && digits(at + 6, 2) {
                    &value[at + 6..at + 8]
                } else {
                    "00"
                };
                return format!("{}:{}", &value[at..at + 5], seconds);
            }
        }
    }
    for at in 0..bytes.len() {
        if digits(at, 4)
            && bytes.get(at + 4) == Some(&b'-')
            && digits(at + 5, 2)
            && bytes.get(at + 7) == Some(&b'-')
            && digits(at + 8, 2)
        {
            return value[at..at + 10].to_owned();
        }
    }
    value.to_owned()
}

/// App `pickFirstNonEmptySeries`：第一条有正数的序列。
fn first_positive(series: &[&Value]) -> Option<Vec<f64>> {
    series
        .iter()
        .find(|values| {
            values
                .as_array()
                .is_some_and(|items| items.iter().any(|v| finite(v).unwrap_or(0.0) > 0.0))
        })
        .map(|values| numbers(values))
}

/// App `sumSeries`：按位置相加；都没有时为空。
fn sum_series(series: &[&Value]) -> Option<Vec<f64>> {
    let length = series
        .iter()
        .map(|values| array(values).len())
        .max()
        .unwrap_or(0);
    (length > 0).then(|| {
        (0..length)
            .map(|index| {
                series
                    .iter()
                    .map(|values| array(values).get(index).and_then(finite).unwrap_or(0.0))
                    .sum()
            })
            .collect()
    })
}

/// App `normalizeNumberSeries`：不是有限数的为 0。
fn numbers(values: &Value) -> Vec<f64> {
    array(values)
        .iter()
        .map(|value| finite(value).unwrap_or(0.0))
        .collect()
}

fn strings(values: &Value) -> Vec<String> {
    array(values)
        .iter()
        .map(|value| match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        })
        .collect()
}

fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
