//! 时间与字节的单位换算（docs/specs/rust-cli/constants.md §2、§3）：业务代码里不散写 1000、60、3600、86400、1024。
//!
//! 类型是 `u64`（字节是 `usize`）；浮点运算用 `_F64` 的那一个，有符号运算在使用处 `as i64`（值都远小于 `i64::MAX`）。

/// 一秒的毫秒数。
pub const MS_PER_SECOND: u64 = 1_000;
/// 浮点运算里一秒的毫秒数。
pub const MS_PER_SECOND_F64: f64 = 1_000.0;
/// 一分钟的秒数。
pub const SECONDS_PER_MINUTE: u64 = 60;
/// 一小时的分钟数。
pub const MINUTES_PER_HOUR: u64 = 60;
/// 一天的小时数。
pub const HOURS_PER_DAY: u64 = 24;
/// 一周的天数。
pub const DAYS_PER_WEEK: u64 = 7;

/// 一小时的秒数。
pub const SECONDS_PER_HOUR: u64 = SECONDS_PER_MINUTE * MINUTES_PER_HOUR;
/// 一天的秒数（JS 时间戳按 UTC 日切分，没有闰秒）。
pub const SECONDS_PER_DAY: u64 = SECONDS_PER_HOUR * HOURS_PER_DAY;
/// 一天的分钟数。
pub const MINUTES_PER_DAY: u64 = MINUTES_PER_HOUR * HOURS_PER_DAY;
/// 一分钟的毫秒数。
pub const MS_PER_MINUTE: u64 = MS_PER_SECOND * SECONDS_PER_MINUTE;
/// 一小时的毫秒数。
pub const MS_PER_HOUR: u64 = MS_PER_MINUTE * MINUTES_PER_HOUR;
/// 一天的毫秒数。
pub const MS_PER_DAY: u64 = MS_PER_HOUR * HOURS_PER_DAY;

/// 一秒的微秒数。
pub const MICROS_PER_SECOND: u64 = 1_000_000;
/// 一毫秒的微秒数。
pub const MICROS_PER_MS: u64 = MICROS_PER_SECOND / MS_PER_SECOND;

/// 1 KiB 的字节数。
pub const KIB: usize = 1_024;
/// 1 MiB 的字节数。
pub const MIB: usize = KIB * KIB;

/// 十六进制与十进制的进制（解析 `%XX`、`\uXXXX`、十进制数字）。
pub const HEX_RADIX: u32 = 16;
pub const DECIMAL_RADIX: u32 = 10;
/// 百分比的满值；浮点运算用 `_F64` 的那一个。
pub const PERCENT: u64 = 100;
pub const PERCENT_F64: f64 = 100.0;
/// 保留一位小数：乘它、取整、再除以它。
pub const TENTHS_F64: f64 = 10.0;

/// Windows FILETIME（1601-01-01 起的 100 ns 数）一毫秒的个数。
pub const FILETIME_TICKS_PER_MS: u64 = 10_000;
/// FILETIME 的纪元（1601-01-01）到 Unix 纪元（1970-01-01）的毫秒数：11644473600 秒。
pub const FILETIME_EPOCH_OFFSET_MS: u64 = 11_644_473_600 * MS_PER_SECOND;

/// 毫秒四舍五入到秒：`(ms + 500) / 1000`（加法饱和，`u64::MAX` 附近不溢出）。
pub const fn round_ms_to_seconds(ms: u64) -> u64 {
    ms.saturating_add(MS_PER_SECOND / 2) / MS_PER_SECOND
}

/// 时长拆成天、时、分、秒（秒四舍五入，进位到分、时、天）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DurationParts {
    pub days: u64,
    pub hours: u64,
    pub minutes: u64,
    pub seconds: u64,
}

/// 毫秒拆成 [`DurationParts`]：秒按 JS 的 `Math.round` 取（不足一分钟的余数四舍五入后最多 60），满 60 秒、60 分、
/// 24 时各进一位。以前 agents 的同伴列表与 kernel-api 的时长写法各写一份（F122 收拢）。
pub const fn split_ms(ms: u64) -> DurationParts {
    let mut days = ms / MS_PER_DAY;
    let mut hours = ms % MS_PER_DAY / MS_PER_HOUR;
    let mut minutes = ms % MS_PER_HOUR / MS_PER_MINUTE;
    let mut seconds = round_ms_to_seconds(ms % MS_PER_MINUTE);
    if seconds == SECONDS_PER_MINUTE {
        seconds = 0;
        minutes += 1;
    }
    if minutes == MINUTES_PER_HOUR {
        minutes = 0;
        hours += 1;
    }
    if hours == HOURS_PER_DAY {
        hours = 0;
        days += 1;
    }
    DurationParts {
        days,
        hours,
        minutes,
        seconds,
    }
}

#[cfg(test)]
#[path = "units_tests.rs"]
mod tests;
