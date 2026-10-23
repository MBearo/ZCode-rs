//! cron 表达式：解析、下次触发时间、抖动与人读写法（docs/specs/rust-cli/tools-more.md §10.3）。
//!
//! 规则按本地时区计算；这里按注入的偏移函数换算（`Clock::utc_offset_minutes`），内部用“本地墙上时间的分钟数”迭代。

/// 每段的取值范围：分、时、日、月、星期（0 = 周日；`7` 也是周日）。
const RANGES: [(u32, u32); 5] = [(0, 59), (0, 23), (1, 31), (1, 12), (0, 6)];
/// 找下次时间的迭代上限（366 天的分钟数）。
const MAX_STEPS: u32 = 527_040;
pub(crate) const MINUTE_MS: i64 = 60_000;

/// 抖动配置的默认值。
const RECURRING_FRAC: f64 = 0.5;
const RECURRING_CAP_MS: f64 = 1_800_000.0;
const ONE_SHOT_MAX_MS: f64 = 90_000.0;
const ONE_SHOT_MINUTE_MOD: u32 = 30;
/// 循环任务创建满 7 天后最后触发一次再删除。
pub(crate) const RECURRING_MAX_AGE_MS: u64 = 604_800_000;
pub(crate) const CACHE_LEAD_MS: i64 = 15_000;
/// 5 分钟的 prompt cache 窗口。
pub(crate) const CACHE_WINDOW_MS: i64 = 300_000;

/// 展开后的五段（升序、去重）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CronFields {
    minute: Vec<u32>,
    hour: Vec<u32>,
    day_of_month: Vec<u32>,
    month: Vec<u32>,
    day_of_week: Vec<u32>,
}

fn digits(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    // JS parseInt 对很长的数字串给出很大的值，超出范围同样判为不合法。
    Some(text.parse::<u32>().unwrap_or(u32::MAX))
}

/// 展开一段：`*`、`*/N`、`N-M[/S]`、`N`，逗号分隔。
fn expand(field: &str, (min, max): (u32, u32)) -> Option<Vec<u32>> {
    let day_of_week = min == 0 && max == 6;
    let mut out = std::collections::BTreeSet::new();
    for part in field.split(',') {
        if let Some(rest) = part.strip_prefix('*') {
            let step = match rest.strip_prefix('/') {
                Some(step) => digits(step)?,
                None if rest.is_empty() => 1,
                None => return None,
            };
            if step < 1 {
                return None;
            }
            out.extend((min..=max).step_by(step as usize));
            continue;
        }
        if let Some((low, high)) = part.split_once('-') {
            let (high, step) = match high.split_once('/') {
                Some((high, step)) => (digits(high)?, digits(step)?),
                None => (digits(high)?, 1),
            };
            let low = digits(low)?;
            let top = if day_of_week { 7 } else { max };
            if low > high || step < 1 || low < min || high > top {
                return None;
            }
            for value in (low..=high).step_by(step as usize) {
                out.insert(if day_of_week && value == 7 { 0 } else { value });
            }
            continue;
        }
        let mut value = digits(part)?;
        if day_of_week && value == 7 {
            value = 0;
        }
        if value < min || value > max {
            return None;
        }
        out.insert(value);
    }
    (!out.is_empty()).then(|| out.into_iter().collect())
}

/// 解析：5 段，否则 None。
pub(crate) fn parse(expr: &str) -> Option<CronFields> {
    let parts: Vec<&str> = expr.split_whitespace().collect();
    let [minute, hour, dom, month, dow] = parts.as_slice() else {
        return None;
    };
    Some(CronFields {
        minute: expand(minute, RANGES[0])?,
        hour: expand(hour, RANGES[1])?,
        day_of_month: expand(dom, RANGES[2])?,
        month: expand(month, RANGES[3])?,
        day_of_week: expand(dow, RANGES[4])?,
    })
}

/// 1970-01-01 起的天数 → (年, 月, 日)（Howard Hinnant 的 civil_from_days）。
fn civil(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// (年, 月, 日) → 1970-01-01 起的天数（days_from_civil）。
fn days_from(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let month = i64::from(month);
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn local_minutes(utc_ms: u64, offset: &dyn Fn(u64) -> i32) -> i64 {
    let local = utc_ms as i64 + i64::from(offset(utc_ms)) * MINUTE_MS;
    local.div_euclid(MINUTE_MS)
}

/// 本地时间的各部分：(年, 月, 日, 时, 分, 秒)。
pub(crate) fn local_parts(
    utc_ms: u64,
    offset: &dyn Fn(u64) -> i32,
) -> (i64, u32, u32, u32, u32, u32) {
    let local = utc_ms as i64 + i64::from(offset(utc_ms)) * MINUTE_MS;
    let seconds = local.div_euclid(1_000);
    let (year, month, day) = civil(seconds.div_euclid(86_400));
    let of_day = seconds.rem_euclid(86_400) as u32;
    (
        year,
        month,
        day,
        of_day / 3_600,
        of_day / 60 % 60,
        of_day % 60,
    )
}

/// 本地墙上时间（分钟数）→ UTC 毫秒：先按该时刻附近的偏移换算，再用换算结果的偏移校正一次。
fn to_utc(local: i64, offset: &dyn Fn(u64) -> i32) -> u64 {
    let local_ms = local * MINUTE_MS;
    let guess = local_ms - i64::from(offset(local_ms.max(0) as u64)) * MINUTE_MS;
    let utc = local_ms - i64::from(offset(guess.max(0) as u64)) * MINUTE_MS;
    utc.max(0) as u64
}

/// 下次时间：严格晚于 `from_ms` 的下一个整分钟起逐分钟找；月、日、时不匹配时整段跳过。
pub(crate) fn next_run(
    fields: &CronFields,
    from_ms: u64,
    offset: &dyn Fn(u64) -> i32,
) -> Option<u64> {
    let dom_wild = fields.day_of_month.len() == 31;
    let dow_wild = fields.day_of_week.len() == 7;
    let mut t = local_minutes(from_ms, offset) + 1;
    for _ in 0..MAX_STEPS {
        let days = t.div_euclid(1_440);
        let minute_of_day = t.rem_euclid(1_440);
        let (year, month, day) = civil(days);
        if !fields.month.contains(&month) {
            let (year, month) = if month == 12 {
                (year + 1, 1)
            } else {
                (year, month + 1)
            };
            t = days_from(year, month, 1) * 1_440;
            continue;
        }
        let weekday = (days + 4).rem_euclid(7) as u32;
        let day_matches = match (dom_wild, dow_wild) {
            (true, true) => true,
            (true, false) => fields.day_of_week.contains(&weekday),
            (false, true) => fields.day_of_month.contains(&day),
            (false, false) => {
                fields.day_of_month.contains(&day) || fields.day_of_week.contains(&weekday)
            }
        };
        if !day_matches {
            t = (days + 1) * 1_440;
            continue;
        }
        let hour = (minute_of_day / 60) as u32;
        if !fields.hour.contains(&hour) {
            t = days * 1_440 + (i64::from(hour) + 1) * 60;
            continue;
        }
        if !fields.minute.contains(&((minute_of_day % 60) as u32)) {
            t += 1;
            continue;
        }
        return Some(to_utc(t, offset));
    }
    None
}

/// 严格晚于 `from_ms` 的下次时间。
pub(crate) fn next_after(expr: &str, from_ms: u64, offset: &dyn Fn(u64) -> i32) -> Option<u64> {
    next_run(&parse(expr)?, from_ms, offset)
}

/// 任务 ID 前 8 位十六进制 / 2³²；不是十六进制时为 0。
fn fraction(id: &str) -> f64 {
    let head: String = id.chars().take(8).collect();
    u32::from_str_radix(&head, 16).map_or(0.0, |value| f64::from(value) / 4_294_967_296.0)
}

/// `*/N * * * *`。
fn every_n_minutes(expr: &str) -> bool {
    let parts: Vec<&str> = expr.split(' ').collect();
    matches!(parts.as_slice(), [minute, "*", "*", "*", "*"]
        if minute.strip_prefix("*/").and_then(digits).is_some())
}

/// 小数毫秒向上取整：`now >= at` 与 `now >= 小数` 等价。
fn ceil_ms(value: f64) -> u64 {
    value.max(0.0).ceil() as u64
}

/// 循环任务的下次触发时间（含抖动）。
pub(crate) fn recurring_next(
    expr: &str,
    from_ms: u64,
    id: &str,
    offset: &dyn Fn(u64) -> i32,
) -> Option<u64> {
    let next = next_after(expr, from_ms, offset)?;
    let Some(after) = next_after(expr, next, offset) else {
        return Some(next);
    };
    let period = after as i64 - next as i64;
    if every_n_minutes(expr)
        && CACHE_LEAD_MS < period
        && period >= CACHE_WINDOW_MS
        && period - CACHE_LEAD_MS < CACHE_WINDOW_MS
    {
        return Some((from_ms as i64 + period - CACHE_LEAD_MS).max(0) as u64);
    }
    let jitter = (fraction(id) * RECURRING_FRAC * period as f64).min(RECURRING_CAP_MS);
    Some(ceil_ms(next as f64 + jitter))
}

/// 一次性任务落在 :00 / :30 时提前最多 90 秒，不早于 `from_ms`。
pub(crate) fn one_shot_next(
    expr: &str,
    from_ms: u64,
    id: &str,
    offset: &dyn Fn(u64) -> i32,
) -> Option<u64> {
    let next = next_after(expr, from_ms, offset)?;
    let minute = local_minutes(next, offset).rem_euclid(60) as u32;
    if !minute.is_multiple_of(ONE_SHOT_MINUTE_MOD) {
        return Some(next);
    }
    let lead = fraction(id) * ONE_SHOT_MAX_MS;
    Some(ceil_ms(next as f64 - lead).max(from_ms))
}

const DAY_NAMES: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

/// `toLocaleTimeString('en-US', {hour: 'numeric', minute: '2-digit'})` 的写法（时间与
/// AM/PM 之间是普通空格）。
fn clock_time(minute: u32, hour: u32) -> String {
    let (display, period) = match hour {
        0 => (12, "AM"),
        1..=11 => (hour, "AM"),
        12 => (12, "PM"),
        _ => (hour - 12, "PM"),
    };
    format!("{display}:{minute:02} {period}")
}

/// 人读写法（本地时间，不带 `utc`）：常见写法转成英文，其余原样返回。
pub(crate) fn human(expr: &str) -> String {
    let parts: Vec<&str> = expr.split_whitespace().collect();
    let [minute, hour, dom, month, dow] = parts.as_slice() else {
        return expr.to_owned();
    };
    let step = |field: &str| field.strip_prefix("*/").and_then(digits);
    let rest_wild = *dom == "*" && *month == "*" && *dow == "*";
    if *hour == "*" && rest_wild {
        if *minute == "*" {
            return "Every minute".to_owned();
        }
        if let Some(n) = step(minute) {
            return if n == 1 {
                "Every minute".to_owned()
            } else {
                format!("Every {n} minutes")
            };
        }
    }
    let minute_value = digits(minute);
    if let (Some(m), true, true) = (minute_value, *hour == "*", rest_wild) {
        return if m == 0 {
            "Every hour".to_owned()
        } else {
            format!("Every hour at :{m:02}")
        };
    }
    if let (Some(m), Some(n), true) = (minute_value, step(hour), rest_wild) {
        let suffix = if m == 0 {
            String::new()
        } else {
            format!(" at :{m:02}")
        };
        return if n == 1 {
            format!("Every hour{suffix}")
        } else {
            format!("Every {n} hours{suffix}")
        };
    }
    let (Some(m), Some(h)) = (minute_value, digits(hour)) else {
        return expr.to_owned();
    };
    if rest_wild {
        return format!("Every day at {}", clock_time(m, h));
    }
    if *dom == "*"
        && *month == "*"
        && dow.len() == 1
        && let Some(index) = digits(dow)
    {
        let name = DAY_NAMES[(index % 7) as usize];
        return format!("Every {name} at {}", clock_time(m, h));
    }
    if *dom == "*" && *month == "*" && *dow == "1-5" {
        return format!("Weekdays at {}", clock_time(m, h));
    }
    expr.to_owned()
}

#[cfg(test)]
#[path = "cron_expr_tests.rs"]
mod tests;
