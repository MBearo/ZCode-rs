//! 公历日期与 Unix 纪元以来的天数互转（docs/specs/rust-cli/constants.md §2）：Howard Hinnant 的
//! `days_from_civil` / `civil_from_days`，按 400 年的周期（146097 天）计算，适用于任何年份。
//!
//! 以前 base、cli、kernel-api、node-store、session、tools、tui 各写一份（F122 收拢，调用方逐批改过来）。另有月长、闰年、
//! 严格的 `YYYY-MM-DD`、月份缩写与 12 小时制。

/// 一年的天数（不含闰日）。
const DAYS_PER_YEAR: i64 = 365;
/// 闰年的间隔、不闰的世纪、闰的世纪（格里历的规则）。
const LEAP_CYCLE: i64 = 4;
const CENTURY: i64 = 100;
const YEARS_PER_ERA: i64 = 400;
/// 一个 400 年周期的天数。
const DAYS_PER_ERA: i64 = 146_097;
/// 周期内的修正：4 年（不含闰日）、100 年（含 24 个闰日）的天数。
const DAYS_PER_LEAP_CYCLE: i64 = DAYS_PER_YEAR * LEAP_CYCLE;
const DAYS_PER_CENTURY: i64 = 36_524;
/// 0000-03-01 到 1970-01-01 的天数（算法从 3 月起算一年，闰日落在年末）。
const UNIX_EPOCH_DAY: i64 = 719_468;
/// 3 月起算的月份：每 5 个月 153 天（31、30、31、30、31），舍入的偏移是 2。
const MONTHS_PER_GROUP: i64 = 5;
const DAYS_PER_GROUP: i64 = 153;
const GROUP_ROUNDING: i64 = 2;
/// 3 月、2 月与一年的月数。
const MARCH: i64 = 3;
const FEBRUARY: u32 = 2;
const MONTHS_PER_YEAR: i64 = 12;
/// 1970-01-01 是星期四（0 = 星期日）。
const EPOCH_WEEKDAY: i64 = 4;
const DAYS_PER_WEEK: i64 = crate::units::DAYS_PER_WEEK as i64;

/// (年, 月 1–12, 日 1–31) → 1970-01-01 以来的天数（之前为负）。月份超出 1–12 时结果没有意义（调用方先校验）。
pub const fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let month = month as i64;
    let year = if month <= FEBRUARY as i64 {
        year - 1
    } else {
        year
    };
    let era = year.div_euclid(YEARS_PER_ERA);
    let year_of_era = year.rem_euclid(YEARS_PER_ERA);
    let month_from_march = (month - MARCH).rem_euclid(MONTHS_PER_YEAR);
    let day_of_year =
        (DAYS_PER_GROUP * month_from_march + GROUP_ROUNDING) / MONTHS_PER_GROUP + day as i64 - 1;
    let day_of_era = year_of_era * DAYS_PER_YEAR + year_of_era / LEAP_CYCLE - year_of_era / CENTURY
        + day_of_year;
    era * DAYS_PER_ERA + day_of_era - UNIX_EPOCH_DAY
}

/// 1970-01-01 以来的天数 → (年, 月 1–12, 日 1–31)。
pub const fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + UNIX_EPOCH_DAY;
    let era = shifted.div_euclid(DAYS_PER_ERA);
    let day_of_era = shifted.rem_euclid(DAYS_PER_ERA);
    let year_of_era = (day_of_era - day_of_era / DAYS_PER_LEAP_CYCLE
        + day_of_era / DAYS_PER_CENTURY
        - day_of_era / (DAYS_PER_ERA - 1))
        / DAYS_PER_YEAR;
    let day_of_year = day_of_era
        - (DAYS_PER_YEAR * year_of_era + year_of_era / LEAP_CYCLE - year_of_era / CENTURY);
    let month_from_march = (MONTHS_PER_GROUP * day_of_year + GROUP_ROUNDING) / DAYS_PER_GROUP;
    let day =
        day_of_year - (DAYS_PER_GROUP * month_from_march + GROUP_ROUNDING) / MONTHS_PER_GROUP + 1;
    let month = (month_from_march + MARCH - 1).rem_euclid(MONTHS_PER_YEAR) + 1;
    let year = year_of_era + era * YEARS_PER_ERA + (month <= FEBRUARY as i64) as i64;
    (year, month as u32, day as u32)
}

/// 一月与十二月（月份从 1 数）。
pub const JANUARY: u32 = 1;
pub const DECEMBER: u32 = MONTHS_PER_YEAR as u32;
/// 一个月最多的天数。
pub const MAX_DAY_OF_MONTH: u32 = 31;
/// 2 月以外的小月与它们的天数。
const SHORT_MONTHS: [u32; 4] = [4, 6, 9, 11];
const SHORT_MONTH_DAYS: u32 = 30;
/// 2 月平年与闰年的天数。
const FEBRUARY_DAYS: u32 = 28;
const FEBRUARY_LEAP_DAYS: u32 = 29;
/// 月份的英文缩写（`MONTH_ABBREVIATIONS[月 - 1]`）。
pub const MONTH_ABBREVIATIONS: [&str; MONTHS_PER_YEAR as usize] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
/// 正午的钟点（12 小时制里 0 点与 12 点都写 12）。
pub const NOON: u32 = 12;

/// 格里历的闰年。
pub const fn is_leap_year(year: i64) -> bool {
    (year % LEAP_CYCLE == 0 && year % CENTURY != 0) || year % YEARS_PER_ERA == 0
}

/// `year` 年 `month` 月（1–12）的天数。
pub const fn days_in_month(year: i64, month: u32) -> u32 {
    if month == FEBRUARY {
        return if is_leap_year(year) {
            FEBRUARY_LEAP_DAYS
        } else {
            FEBRUARY_DAYS
        };
    }
    let mut index = 0;
    while index < SHORT_MONTHS.len() {
        if SHORT_MONTHS[index] == month {
            return SHORT_MONTH_DAYS;
        }
        index += 1;
    }
    MAX_DAY_OF_MONTH
}

/// 1970-01-01 以来的天数 → `YYYY-MM-DD`。
pub fn format_date(days: i64) -> String {
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// 严格的 `YYYY-MM-DD`（4、2、2 位数字，月 1–12，日 1–31，不查月长）→ 1970-01-01 以来的天数。
pub fn parse_date(date: &str) -> Option<i64> {
    let mut parts = date.split('-');
    let (year, month, day) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some()
        || year.len() != iso::YEAR.len()
        || month.len() != iso::MONTH.len()
        || day.len() != iso::DAY.len()
    {
        return None;
    }
    let (year, month, day): (i64, u32, u32) =
        (year.parse().ok()?, month.parse().ok()?, day.parse().ok()?);
    if !(JANUARY..=DECEMBER).contains(&month) || !(1..=MAX_DAY_OF_MONTH).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day))
}

/// 24 小时制的钟点 → 12 小时制的（钟点, `AM` / `PM`）：0 点是 12 AM，12 点是 12 PM。
pub const fn twelve_hour(hour: u32) -> (u32, &'static str) {
    if hour == 0 {
        (NOON, "AM")
    } else if hour < NOON {
        (hour, "AM")
    } else if hour == NOON {
        (NOON, "PM")
    } else {
        (hour - NOON, "PM")
    }
}

/// 1970-01-01 以来的天数是星期几（0 = 星期日 … 6 = 星期六）。
pub const fn weekday(days: i64) -> u32 {
    (days + EPOCH_WEEKDAY).rem_euclid(DAYS_PER_WEEK) as u32
}

/// ISO 8601 定长写法 `YYYY-MM-DDTHH:MM:SS` 的布局：各段的字节范围与分隔符的位置（解析在调用方，规则各有不同）。
pub mod iso {
    use std::ops::Range;

    pub const YEAR: Range<usize> = 0..4;
    pub const MONTH: Range<usize> = 5..7;
    pub const DAY: Range<usize> = 8..10;
    pub const HOUR: Range<usize> = 11..13;
    pub const MINUTE: Range<usize> = 14..16;
    pub const SECOND: Range<usize> = 17..19;
    /// 日期里两个 `-` 的位置。
    pub const DATE_DASHES: [usize; 2] = [4, 7];
    /// 日期与时间之间的 `T` 的位置。
    pub const TIME_MARKER: usize = 10;
    /// 时间里两个 `:` 的位置。
    pub const TIME_COLONS: [usize; 2] = [13, 16];
    /// `YYYY-MM-DD` 的长度。
    pub const DATE_LEN: usize = DAY.end;
    /// `YYYY-MM-DDTHH:MM:SS` 的长度（后面是小数与时区）。
    pub const DATETIME_LEN: usize = SECOND.end;
}

#[cfg(test)]
#[path = "civil_tests.rs"]
mod tests;
