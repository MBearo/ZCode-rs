//! 时长、时刻、计数与百分比的写法（docs/specs/rust-cli/constants.md §5.3）：只写最大的单位、时分秒、一天里的时刻、
//! `1.2k`、一位小数的百分比。以前 goal、session_rows、retry_status、i18n、export、status_line、context、footer_stats
//! 各写一份（F122 第二阶段收拢）；时间单位来自 base `units`。
use rust_cli_base::units;

/// 时长的单位（从小到大）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Second,
    Minute,
    Hour,
    Day,
}

impl Unit {
    /// 紧跟在数字后面的缩写：`s`、`m`、`h`、`d`。
    pub fn short(self) -> &'static str {
        match self {
            Self::Second => "s",
            Self::Minute => "m",
            Self::Hour => "h",
            Self::Day => "d",
        }
    }

    /// 英文的单数名字：`second`、`minute`、`hour`、`day`。
    pub fn english(self) -> &'static str {
        match self {
            Self::Second => "second",
            Self::Minute => "minute",
            Self::Hour => "hour",
            Self::Day => "day",
        }
    }
}

/// 秒数只取最大的一个单位，向下取整：不到一分钟是秒，不到一小时是分，不到一天是时，其余是天。
pub fn largest_unit(seconds: u64) -> (u64, Unit) {
    if seconds < units::SECONDS_PER_MINUTE {
        (seconds, Unit::Second)
    } else if seconds < units::SECONDS_PER_HOUR {
        (seconds / units::SECONDS_PER_MINUTE, Unit::Minute)
    } else if seconds < units::SECONDS_PER_DAY {
        (seconds / units::SECONDS_PER_HOUR, Unit::Hour)
    } else {
        (seconds / units::SECONDS_PER_DAY, Unit::Day)
    }
}

/// 毫秒数只写最大的一个单位（向下取整到秒）：`42s`、`35m`、`2h`、`1d`。
pub fn largest_unit_text(ms: u64) -> String {
    let (value, unit) = largest_unit(ms / units::MS_PER_SECOND);
    format!("{value}{}", unit.short())
}

/// 秒数拆成（时, 分, 秒）；时不按天进位。
pub fn hours_minutes_seconds(seconds: u64) -> (u64, u64, u64) {
    (
        seconds / units::SECONDS_PER_HOUR,
        seconds % units::SECONDS_PER_HOUR / units::SECONDS_PER_MINUTE,
        seconds % units::SECONDS_PER_MINUTE,
    )
}

/// 本地时间（毫秒）在那一天里的（时, 分, 秒）。
pub fn time_of_day(local_ms: u64) -> (u64, u64, u64) {
    hours_minutes_seconds(local_ms / units::MS_PER_SECOND % units::SECONDS_PER_DAY)
}

/// 一千及以上写成一位小数的 `k`（`1.2k`），以下原样。
pub fn thousands(count: u64) -> String {
    /// `k` 的倍数。
    const THOUSAND: u64 = 1_000;
    if count >= THOUSAND {
        format!("{:.1}k", count as f64 / THOUSAND as f64)
    } else {
        count.to_string()
    }
}

/// 百分比的分母。
pub const PERCENT: u64 = 100;
/// 浮点运算里百分比的分母。
pub const PERCENT_F64: f64 = 100.0;

/// 四舍五入到一位小数，整数也写 `.0`（`12.3`、`40.0`）；负数按 0。
pub fn tenths(value: f64) -> String {
    /// 一位小数的倍数。
    const TENTHS: f64 = 10.0;
    format!("{:.1}", (value * TENTHS).round().max(0.0) / TENTHS)
}

#[cfg(test)]
#[path = "quantities_tests.rs"]
mod tests;
