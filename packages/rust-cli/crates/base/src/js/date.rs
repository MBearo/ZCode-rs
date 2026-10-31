// 复制自 rust/crates/kit/src/js/date.rs（feat/rs 79b6691c55）。
//! JavaScript `Date` 的文本形式与日期常量。

use crate::units::{
    MINUTES_PER_HOUR, MS_PER_HOUR, MS_PER_MINUTE, MS_PER_SECOND, SECONDS_PER_MINUTE,
};

/// 一天的毫秒数（JS 时间戳按 UTC 日切分）。F58：以前 tui 的用量统计（u64）与 node-store 的用量查询（i64）各写一份；
/// F122 起定义在 [`crate::units`]，这里保留旧名字。
pub use crate::units::MS_PER_DAY as DAY_MS;

/// JS `new Date(ms).toISOString()`.
pub fn iso_timestamp(ms: u64) -> String {
    let rest = ms % DAY_MS;
    let (year, month, day) = crate::civil::civil_from_days((ms / DAY_MS) as i64);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / MS_PER_HOUR,
        rest / MS_PER_MINUTE % MINUTES_PER_HOUR,
        rest / MS_PER_SECOND % SECONDS_PER_MINUTE,
        rest % MS_PER_SECOND
    )
}

#[cfg(test)]
#[path = "date_tests.rs"]
mod tests;
