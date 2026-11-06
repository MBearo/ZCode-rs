//! 套餐用量的日期（docs/specs/rust-cli/platform.md §11.3、§11.5）：本地日期与天数的换算，App 用 JS `Date` 解析的时刻。

use rust_cli_base::js::date::DAY_MS;
use rust_cli_kernel_api::clock::{Clock, civil_date};

/// 1970-01-01 起的天数 → `YYYY-MM-DD`。
pub(super) fn date_of_day(day: i64) -> String {
    // civil_date 只收非负毫秒；用量的日期都在 1970 年之后。
    civil_date(u64::try_from(day).unwrap_or(0) * DAY_MS)
}

/// `YYYY-MM-DD` → 1970-01-01 起的天数（Howard Hinnant `days_from_civil`）；不是合法日期时为 None。照 V8：日只查 1–31，
/// 超出当月的天顺延到下个月（`2026-02-30` 是 3 月 2 日）。
pub(super) fn day_of_date(date: &str) -> Option<i64> {
    let bytes = date.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let number = |range: std::ops::Range<usize>| -> Option<i64> {
        let text = date.get(range)?;
        text.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| text.parse().ok())
            .flatten()
    };
    days_from_civil(number(0..4)?, number(5..7)?, number(8..10)?)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// 星期几（0 是周日，JS `getUTCDay`）。
pub(super) fn weekday(day: i64) -> i64 {
    (day + 4).rem_euclid(7)
}

/// 注入时钟的本地今天（1970-01-01 起的天数；App 用 `Intl` 与系统时区，D-U-6）。
pub(super) fn today(clock: &dyn Clock) -> i64 {
    let now = clock.now_ms();
    let offset = i64::from(clock.utc_offset_minutes(now)) * 60_000;
    let local = i64::try_from(now)
        .unwrap_or(i64::MAX)
        .saturating_add(offset);
    local.div_euclid(i64::try_from(DAY_MS).unwrap_or(1))
}

/// App 的 `new Date(text)` / `Date.parse(text)`：ISO 形式的日期与时刻（日期与时刻之间可以是空格，App
/// `parseBigModelLocalDateTime` 换成 `T`）。带 `Z` 或偏移的按它；只有日期的按 UTC 零点；带时刻、不带时区的按本地时间
/// （`local_offset` 给出某一刻的本地偏移，分钟）。解析不了时为 None。
pub(super) fn parse_time(text: &str, local_offset: &(dyn Fn(u64) -> i32 + Sync)) -> Option<u64> {
    let text = text.trim();
    let (date, rest) = (text.get(..10)?, &text[10..]);
    let day = day_of_date(date)?;
    let midnight = day.checked_mul(i64::try_from(DAY_MS).ok()?)?;
    if rest.is_empty() {
        return u64::try_from(midnight).ok();
    }
    let rest = rest.strip_prefix(['T', ' '])?;
    let (clock, zone) = split_zone(rest)?;
    let millis = clock_millis(clock)?;
    let wall = midnight.checked_add(millis)?;
    let utc = match zone {
        Zone::Utc => wall,
        Zone::Offset(minutes) => wall - minutes * 60_000,
        Zone::Local => {
            // 本地时刻：先按 UTC 当作那一刻取偏移，再按偏移换算一次（夏令时边上与 JS 一样取换算后的那一刻）。
            let guess = u64::try_from(wall).ok()?;
            let first = i64::from(local_offset(guess));
            let shifted = u64::try_from(wall - first * 60_000).ok()?;
            wall - i64::from(local_offset(shifted)) * 60_000
        }
    };
    u64::try_from(utc).ok()
}

enum Zone {
    Utc,
    Offset(i64),
    Local,
}

fn split_zone(text: &str) -> Option<(&str, Zone)> {
    if let Some(clock) = text.strip_suffix(['Z', 'z']) {
        return Some((clock, Zone::Utc));
    }
    if let Some(at) = text.rfind(['+', '-']) {
        let (clock, zone) = text.split_at(at);
        let sign = if zone.starts_with('-') { -1 } else { 1 };
        let digits: String = zone[1..].chars().filter(|c| *c != ':').collect();
        if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let hours: i64 = digits[..2].parse().ok()?;
        let minutes: i64 = digits[2..].parse().ok()?;
        return Some((clock, Zone::Offset(sign * (hours * 60 + minutes))));
    }
    Some((text, Zone::Local))
}

/// `HH:mm`、`HH:mm:ss` 或 `HH:mm:ss.sss` → 当天的毫秒。
fn clock_millis(text: &str) -> Option<i64> {
    let (main, fraction) = match text.split_once('.') {
        Some((main, fraction)) => (main, Some(fraction)),
        None => (text, None),
    };
    let parts: Vec<&str> = main.split(':').collect();
    if !(2..=3).contains(&parts.len())
        || parts
            .iter()
            .any(|part| part.len() != 2 || !part.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let hours: i64 = parts[0].parse().ok()?;
    let minutes: i64 = parts[1].parse().ok()?;
    let seconds: i64 = parts.get(2).map_or(Some(0), |s| s.parse().ok())?;
    // V8 收 `24:00:00`（第二天零点），不收 24 点以后的。
    if hours > 24 || minutes > 59 || seconds > 59 || (hours == 24 && minutes + seconds > 0) {
        return None;
    }
    let millis = match fraction {
        None => 0,
        Some(digits) if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) => {
            let padded = format!("{:0<3}", &digits[..digits.len().min(3)]);
            padded.parse().ok()?
        }
        Some(_) => return None,
    };
    Some(((hours * 60 + minutes) * 60 + seconds) * 1000 + millis)
}

#[cfg(test)]
#[path = "time_tests.rs"]
mod tests;
