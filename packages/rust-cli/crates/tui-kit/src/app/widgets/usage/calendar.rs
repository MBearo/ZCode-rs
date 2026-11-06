//! 本地日历（Scala 的 js.Date）：按注入的 [`UtcOffset`] 算本地的年月日、时分与星期，纯函数、可测。
//! 移植自 Puffin 的 shell/usage/data.rs 的日历部分（UsageData.scala）。
use super::UtcOffset;

pub const MINUTE_MS: f64 = 60_000.0;
pub const HOUR_MS: f64 = 3_600_000.0;
pub const DAY_MS: f64 = 24.0 * HOUR_MS;

/// 本地的年月日、时分与星期（0 是周日）。月份 1–12。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Civil {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub weekday: u32,
}

/// 1970-01-01 起的天数（Howard Hinnant 的 days_from_civil）。
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(month);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// 天数的年月日。
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// 天数的星期：0 是周日（1970-01-01 是周四）。
pub fn weekday_of(days: i64) -> u32 {
    (days + 4).rem_euclid(7) as u32
}

/// 时刻 `ms` 在 `zone` 里的本地时间。
pub fn local(ms: f64, zone: &UtcOffset) -> Civil {
    let shifted = ms + f64::from(zone(ms)) * MINUTE_MS;
    let days = (shifted / DAY_MS).floor();
    let rest = shifted - days * DAY_MS;
    let days = days as i64;
    let (year, month, day) = civil_from_days(days);
    Civil {
        year,
        month,
        day,
        hour: (rest / HOUR_MS).floor() as u32,
        minute: ((rest % HOUR_MS) / MINUTE_MS).floor() as u32,
        weekday: weekday_of(days),
    }
}

/// 本地的 `year`-`month`-`day`（月与日可越界，按 js.Date 进位）加 `of_day` 毫秒是哪个时刻。
pub fn from_local(year: i64, month: i64, day: i64, of_day: f64, zone: &UtcOffset) -> f64 {
    let y = year + (month - 1).div_euclid(12);
    let m = ((month - 1).rem_euclid(12) + 1) as u32;
    let days = days_from_civil(y, m, 1) + day - 1;
    let wall = days as f64 * DAY_MS + of_day;
    let guess = wall - f64::from(zone(wall)) * MINUTE_MS;
    wall - f64::from(zone(guess)) * MINUTE_MS
}

/// `YYYY-MM-DD` 的天数（Scala 的 `dayOf`：那天的本地零点）。
pub fn day_of(key: &str) -> Option<i64> {
    let parts: Vec<i64> = key
        .split('-')
        .map(str::parse::<i64>)
        .collect::<Result<_, _>>()
        .ok()?;
    match parts.as_slice() {
        [y, m, d] => Some(
            days_from_civil(
                *y + (m - 1).div_euclid(12),
                ((m - 1).rem_euclid(12) + 1) as u32,
                1,
            ) + d
                - 1,
        ),
        _ => None,
    }
}
