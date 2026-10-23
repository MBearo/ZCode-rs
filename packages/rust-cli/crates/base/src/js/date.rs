// 复制自 rust/crates/kit/src/js/date.rs（feat/rs 79b6691c55）。
//! JavaScript `Date` 的文本形式与日期常量。

/// 一天的毫秒数（JS 时间戳按 UTC 日切分）。F58：以前 tui 的用量统计（u64）与 node-store 的用量查询（i64）各写一份。
pub const DAY_MS: u64 = 86_400_000;

/// JS `new Date(ms).toISOString()`.
pub fn iso_timestamp(ms: u64) -> String {
    let days = (ms / DAY_MS) as i64;
    let rest = ms % DAY_MS;
    // Howard Hinnant `civil_from_days`.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3_600_000,
        rest / 60_000 % 60,
        rest / 1000 % 60,
        rest % 1000
    )
}

#[cfg(test)]
#[path = "date_tests.rs"]
mod tests;
