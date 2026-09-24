//! The App usage snapshot (Node `buildAppUsageSnapshot` over `queryAppUsage`).
use super::{DAY_MS, js_number};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Node `AppUsageModelRow`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelRow {
    pub model_id: Option<String>,
    pub total_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub request_count: u64,
}

/// Node `AppUsageToolRow`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolRow {
    pub tool_name: String,
    pub call_count: u64,
    pub error_count: u64,
    pub avg_duration_ms: Option<f64>,
}

/// Node `AppUsageDayRow`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DayRow {
    pub day_index: i64,
    pub total_tokens: u64,
    pub turn_count: u64,
    pub tool_call_count: u64,
}

/// Node `AppUsageDayModelRow`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DayModelRow {
    pub day_index: i64,
    pub model_id: Option<String>,
    pub total_tokens: u64,
}

/// Node `AppUsageQueryResult`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AppRows {
    pub total_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub model_request_count: u64,
    pub model_error_count: u64,
    pub avg_time_to_first_token_ms: Option<f64>,
    pub total_sessions: u64,
    pub total_turns: u64,
    pub avg_turn_duration_ms: Option<f64>,
    pub longest_session_ms: u64,
    pub tool_call_count: u64,
    pub tool_error_count: u64,
    /// Ordered by total tokens, descending.
    pub models: Vec<ModelRow>,
    /// Ordered by call count, descending.
    pub tools: Vec<ToolRow>,
    /// Ordered by day.
    pub days: Vec<DayRow>,
    pub day_models: Vec<DayModelRow>,
}

pub struct SnapshotOptions<'a> {
    pub range: &'a str,
    pub time_zone: &'a str,
    pub tz_offset_ms: i64,
    pub generated_at: u64,
    pub since: i64,
    pub until: i64,
}

fn ratio(part: u64, whole: u64) -> Value {
    if whole > 0 {
        js_number(part as f64 / whole as f64)
    } else {
        0.into()
    }
}

fn optional(value: Option<f64>) -> Value {
    value.map_or(Value::Null, js_number)
}

/// `dayIndex * DAY` is the local midnight read as UTC; its UTC date is the local date.
fn date(day_index: i64) -> String {
    chrono::DateTime::from_timestamp_millis(day_index * DAY_MS)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

fn level(tokens: u64, max: u64) -> u8 {
    if tokens == 0 || max == 0 {
        return 0;
    }
    match tokens as f64 / max as f64 {
        r if r > 0.75 => 4,
        r if r > 0.5 => 3,
        r if r > 0.25 => 2,
        _ => 1,
    }
}

/// Node `buildAppUsageSnapshot`.
pub fn snapshot(rows: &AppRows, opts: &SnapshotOptions) -> Value {
    let cache_denominator = if rows.input_tokens > 0 {
        rows.input_tokens
    } else {
        rows.cache_creation_tokens + rows.cache_read_tokens
    };
    let days: BTreeMap<i64, &DayRow> = rows.days.iter().map(|d| (d.day_index, d)).collect();
    let end = (opts.until + opts.tz_offset_ms).div_euclid(DAY_MS);
    let start = if opts.range != "all" {
        (opts.since + opts.tz_offset_ms).div_euclid(DAY_MS)
    } else {
        rows.days
            .iter()
            .map(|d| d.day_index)
            .chain(rows.day_models.iter().map(|d| d.day_index))
            .min()
            .unwrap_or(end)
    };
    let tokens_on = |day: i64| days.get(&day).map_or(0, |d| d.total_tokens);
    let (mut active, mut current, mut longest, mut running, mut broken) = (0, 0, 0, 0, false);
    let mut day = end;
    while day >= start {
        if tokens_on(day) > 0 {
            active += 1;
            running += 1;
            longest = longest.max(running);
            if !broken {
                current += 1;
            }
        } else {
            broken = true;
            running = 0;
        }
        day -= 1;
    }
    let max = rows.days.iter().map(|d| d.total_tokens).max().unwrap_or(0);
    let mut weeks = vec![];
    let mut week: Vec<Value> = vec![];
    for day in start..=end {
        let row = days.get(&day);
        week.push(
            json!({"date": date(day), "level": level(tokens_on(day), max),
            "totalTokens": tokens_on(day), "turnCount": row.map_or(0, |d| d.turn_count),
            "toolCallCount": row.map_or(0, |d| d.tool_call_count)}),
        );
        if week.len() == 7 {
            weeks.push(json!({"weekIndex": weeks.len(), "days": std::mem::take(&mut week)}));
        }
    }
    if !week.is_empty() {
        week.resize(7, Value::Null);
        weeks.push(json!({"weekIndex": weeks.len(), "days": week}));
    }
    // Node 用 Map 按插入顺序聚合同日模型；这里保留同样的首次出现顺序。
    let mut daily: BTreeMap<i64, Vec<(Option<String>, u64)>> = BTreeMap::new();
    for row in &rows.day_models {
        let models = daily.entry(row.day_index).or_default();
        match models.iter_mut().find(|(id, _)| *id == row.model_id) {
            Some((_, total)) => *total += row.total_tokens,
            None => models.push((row.model_id.clone(), row.total_tokens)),
        }
    }
    let daily_models: Vec<Value> = (start..=end)
        .map(|day| {
            let models = daily.get(&day).map_or(vec![], |models| {
                models
                    .iter()
                    .map(|(id, total)| json!({"modelId": id, "totalTokens": total}))
                    .collect()
            });
            json!({"date": date(day), "models": models})
        })
        .collect();
    let model_total: u64 = rows.models.iter().map(|m| m.total_tokens).sum();
    let models: Vec<Value> = rows
        .models
        .iter()
        .map(|m| {
            json!({"modelId": m.model_id, "totalTokens": m.total_tokens,
                "inputTokens": m.input_tokens, "outputTokens": m.output_tokens,
                "requestCount": m.request_count, "share": ratio(m.total_tokens, model_total)})
        })
        .collect();
    let favorite = models.first().map_or(
        Value::Null,
        |m| json!({"modelId": m["modelId"], "totalTokens": m["totalTokens"], "share": m["share"]}),
    );
    let tools: Vec<Value> = rows
        .tools
        .iter()
        .map(|t| {
            json!({"toolName": t.tool_name, "callCount": t.call_count, "errorCount": t.error_count,
                "errorRate": ratio(t.error_count, t.call_count),
                "avgDurationMs": optional(t.avg_duration_ms)})
        })
        .collect();
    json!({
        "range": opts.range,
        "generatedAt": opts.generated_at,
        "timeZone": opts.time_zone,
        "source": "agent-db",
        "summary": {
            "totalTokens": rows.total_tokens,
            "inputTokens": rows.input_tokens,
            "outputTokens": rows.output_tokens,
            "reasoningTokens": rows.reasoning_tokens,
            "cacheCreationTokens": rows.cache_creation_tokens,
            "cacheReadTokens": rows.cache_read_tokens,
            "cacheHitRate": ratio(rows.cache_read_tokens, cache_denominator),
            "totalSessions": rows.total_sessions,
            "totalTurns": rows.total_turns,
            "toolCallCount": rows.tool_call_count,
            "toolErrorRate": ratio(rows.tool_error_count, rows.tool_call_count),
            "modelErrorRate": ratio(rows.model_error_count, rows.model_request_count),
            "avgTimeToFirstTokenMs": optional(rows.avg_time_to_first_token_ms),
            "avgTurnDurationMs": optional(rows.avg_turn_duration_ms),
            "activeDays": active,
            "currentStreakDays": current,
            "longestSessionMs": rows.longest_session_ms,
            "longestStreakDays": longest,
            "peakDayTokens": max,
            "favoriteModel": favorite,
        },
        "heatmap": {"startDate": date(start), "endDate": date(end), "maxTokens": max, "weeks": weeks},
        "dailyModelUsage": daily_models,
        "models": models,
        "tools": tools,
    })
}
