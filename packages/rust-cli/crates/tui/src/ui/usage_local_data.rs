//! 「This computer」标签页的数据（docs/specs/rust-cli/usage.md §5）：内核给的本机活动统计（[`ActivityStats`]）在这里换成
//! kit 详情页的入参——热力图的周与级别（Puffin 投影 `Activity` 的形状：第一周周日的日期、逐周逐天的 0–4）与连续的天数。
//! 纯函数；画法在 `usage_local.rs`。
use std::collections::{BTreeMap, BTreeSet};

// 一天的毫秒数只定义在 base（F58）。
use rust_cli_base::js::date::DAY_MS;
use rust_cli_kernel_api::clock::civil_date;
use rust_cli_kernel_api::metering::{ActivityDay, ActivityStats};
use rust_cli_tui_kit::app::widgets::usage::Activity;

/// 热力图最多几周（Puffin `heat_rows` 的上限）。
pub const WEEKS: usize = 52;

/// `YYYY-MM-DD` → 自 1970-01-01 的天数（days_from_civil）。
pub fn day_number(date: &str) -> Option<i64> {
    let mut parts = date.splitn(3, '-').map(str::parse::<i64>);
    let (year, month, day) = (
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
    );
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}

fn date_of(day: i64) -> String {
    civil_date(u64::try_from(day).unwrap_or(0) * DAY_MS)
}

/// 连续的天数：以今天为终点往前数的（今天没有消息时是 0）与最长的一段。
pub fn streaks(stats: &ActivityStats) -> (u64, u64) {
    let set: BTreeSet<i64> = stats
        .days
        .iter()
        .filter_map(|d| day_number(&d.date))
        .collect();
    let mut current = 0;
    if let Some(mut day) = day_number(&stats.today) {
        while set.contains(&day) {
            current += 1;
            day -= 1;
        }
    }
    let (mut longest, mut run, mut previous) = (0, 0, None);
    for day in &set {
        run = if previous == Some(day - 1) {
            run + 1
        } else {
            1
        };
        longest = longest.max(run);
        previous = Some(*day);
    }
    (current, longest)
}

/// 有消息的日子的四分位（级别 1–4 的分界）。
fn quartiles(days: &[ActivityDay]) -> Option<[u64; 3]> {
    let mut counts: Vec<u64> = days.iter().map(|d| d.messages).filter(|&n| n > 0).collect();
    if counts.is_empty() {
        return None;
    }
    counts.sort_unstable();
    let at = |fraction: f64| {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let index = (counts.len() as f64 * fraction).floor() as usize;
        counts[index.min(counts.len() - 1)]
    };
    Some([at(0.25), at(0.5), at(0.75)])
}

/// 一天的级别：没有消息 0，其余按四分位 1–4。
fn level(messages: u64, quartiles: Option<[u64; 3]>) -> i8 {
    match quartiles {
        None => 0,
        Some(_) if messages == 0 => 0,
        Some([p25, p50, p75]) => {
            if messages >= p75 {
                4
            } else if messages >= p50 {
                3
            } else if messages >= p25 {
                2
            } else {
                1
            }
        }
    }
}

/// 热力图的数据（kit `Activity`）：[`WEEKS`] 周，最新的一周在最后、含今天，今天之后的日子是 -1；`total` 是消息数，
/// `best_day` 是消息最多的一天的数，`streak` 是到今天为止连续的天数。今天读不出来时没有格子。
pub fn activity(stats: &ActivityStats) -> Activity {
    let total = stats.days.iter().map(|d| d.messages).sum();
    let best_day = stats.days.iter().map(|d| d.messages).max().unwrap_or(0);
    let streak = streaks(stats).0;
    let Some(today) = day_number(&stats.today) else {
        return Activity {
            total,
            best_day,
            streak,
            ..Activity::default()
        };
    };
    let counts: BTreeMap<i64, u64> = stats
        .days
        .iter()
        .filter_map(|d| day_number(&d.date).map(|n| (n, d.messages)))
        .collect();
    let quartiles = quartiles(&stats.days);
    // 1970-01-01 是周四：周日为 0。
    let weekday = (today + 4).rem_euclid(7);
    let start = today - weekday - (WEEKS as i64 - 1) * 7;
    let levels = (0..WEEKS)
        .map(|week| {
            (0..7)
                .map(|row| {
                    let day = start + (week * 7 + row) as i64;
                    if day > today {
                        -1
                    } else {
                        level(counts.get(&day).copied().unwrap_or(0), quartiles)
                    }
                })
                .collect()
        })
        .collect();
    Activity {
        total,
        best_day,
        streak,
        start: date_of(start),
        levels,
    }
}

#[cfg(test)]
#[path = "usage_local_data_tests.rs"]
mod tests;
