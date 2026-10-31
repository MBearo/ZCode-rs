//! 时长、时刻与日期的写法收成一处之前的等价核对（docs/specs/rust-cli/constants.md §5.3）：逐字照抄收拢前的
//! 各份实现，按秒（或按天）扫一遍，与现在的函数逐个比较。
use rust_cli_base::civil;

use super::*;
use crate::ui::i18n::{self, Lang};
use crate::ui::{export, goal, notify, retry_status, session_rows, usage_stats};

/// 收拢前的实现（逐字照抄，只改了名字）。
mod old {
    use crate::ui::i18n::Lang;

    pub fn goal_duration(ms: u64) -> String {
        let seconds = ms / 1000;
        match seconds {
            0..60 => format!("{seconds}s"),
            60..3600 => format!("{}m", seconds / 60),
            3600..86_400 => format!("{}h", seconds / 3600),
            _ => format!("{}d", seconds / 86_400),
        }
    }

    pub fn tokens(count: u64) -> String {
        if count >= 1000 {
            format!("{:.1}k", count as f64 / 1000.0)
        } else {
            count.to_string()
        }
    }

    pub fn tokens_label(count: usize) -> String {
        if count >= 1000 {
            format!("{:.1}k", count as f64 / 1000.0)
        } else {
            count.to_string()
        }
    }

    pub fn relative(lang: Lang, now_ms: u64, at_ms: u64) -> String {
        let seconds = now_ms.saturating_sub(at_ms) / 1000;
        match (lang, seconds) {
            (Lang::En, 0) => "now".to_owned(),
            (Lang::En, 1..60) => format!("{seconds}s ago"),
            (Lang::En, 60..3600) => format!("{}m ago", seconds / 60),
            (Lang::En, 3600..86_400) => format!("{}h ago", seconds / 3600),
            (Lang::En, _) => format!("{}d ago", seconds / 86_400),
            (Lang::Zh, 0) => "刚刚".to_owned(),
            (Lang::Zh, 1..60) => format!("{seconds} 秒前"),
            (Lang::Zh, 60..3600) => format!("{} 分钟前", seconds / 60),
            (Lang::Zh, 3600..86_400) => format!("{} 小时前", seconds / 3600),
            (Lang::Zh, _) => format!("{} 天前", seconds / 86_400),
        }
    }

    pub fn relative_long(lang: Lang, now_ms: u64, at_ms: u64) -> String {
        let seconds = now_ms.saturating_sub(at_ms) / 1000;
        let (value, unit) = match seconds {
            0 => return relative(lang, now_ms, at_ms),
            1..60 => (seconds, "second"),
            60..3600 => (seconds / 60, "minute"),
            3600..86_400 => (seconds / 3600, "hour"),
            _ => (seconds / 86_400, "day"),
        };
        match lang {
            Lang::En if value == 1 => format!("1 {unit} ago"),
            Lang::En => format!("{value} {unit}s ago"),
            Lang::Zh => relative(lang, now_ms, at_ms),
        }
    }

    pub fn timestamp(at_ms: u64, offset_minutes: i32) -> String {
        let local = at_ms as i64 + i64::from(offset_minutes) * 60_000;
        let local = u64::try_from(local).unwrap_or(0);
        let seconds = (local / 1000) % 86_400;
        format!(
            "{} {:02}:{:02}:{:02}",
            rust_cli_kernel_api::clock::civil_date(local),
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    }

    pub fn export_timestamp(now_ms: u64, offset_minutes: i32) -> String {
        let utc = i64::try_from(now_ms).unwrap_or(i64::MAX);
        let local =
            u64::try_from(utc.saturating_add(i64::from(offset_minutes) * 60_000)).unwrap_or(0);
        let date = rust_cli_kernel_api::clock::civil_date(local);
        let seconds = (local / 1000) % 86_400;
        format!(
            "{date}-{:02}{:02}{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    }

    pub fn countdown(ms: u64) -> String {
        let ms = ms.div_ceil(1000) * 1000;
        if ms < 300_000 {
            return rust_cli_kernel_api::metering::format_duration(ms);
        }
        let (days, hours, minutes) = (
            ms / 86_400_000,
            (ms % 86_400_000) / 3_600_000,
            (ms % 3_600_000) / 60_000,
        );
        if days > 0 {
            format!("{days}d")
        } else if hours > 0 {
            format!("{hours}h")
        } else {
            format!("{minutes}m")
        }
    }

    pub fn elapsed_plain(seconds: u64) -> String {
        let (h, m, s) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
        match (h, m) {
            (0, 0) => format!("{s}s"),
            (0, _) => format!("{m}m {s}s"),
            _ => format!("{h}h {m}m {s}s"),
        }
    }

    pub fn elapsed(seconds: u64) -> String {
        let (h, m, s) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
        match (h, m) {
            (0, 0) => format!("{s}s"),
            (0, _) => format!("{m}m {s:02}s"),
            _ => format!("{h}h {m:02}m {s:02}s"),
        }
    }

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

    pub fn notify_id(seed: &str) -> u32 {
        let hash = seed.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
        });
        u32::try_from(hash % 10_000).unwrap_or(0)
    }
}

const SECOND: u64 = 1_000;
const DAY_SECONDS: u64 = 86_400;

/// 0 到 3 天逐秒，再加几个很大的值。
fn seconds() -> impl Iterator<Item = u64> {
    (0..=3 * DAY_SECONDS).chain([
        30 * DAY_SECONDS,
        365 * DAY_SECONDS,
        u64::MAX / SECOND,
        u64::MAX / SECOND - 1,
    ])
}

#[test]
fn largest_unit_texts_are_unchanged() {
    for s in seconds() {
        let ms = s * SECOND;
        assert_eq!(goal::duration(ms), old::goal_duration(ms), "{s}");
        let late = ms.saturating_add(999);
        assert_eq!(goal::duration(late), old::goal_duration(late), "{s}");
        for lang in [Lang::En, Lang::Zh] {
            let now = ms.saturating_add(SECOND / 2);
            assert_eq!(
                session_rows::relative(lang, now, 0),
                old::relative(lang, now, 0),
                "{s}"
            );
            assert_eq!(
                session_rows::relative_long(lang, now, 0),
                old::relative_long(lang, now, 0),
                "{s}"
            );
        }
    }
    // 倒计时按毫秒向上取整到秒，边界在整秒的两侧（旧实现在 `u64::MAX` 附近溢出，不扫那一段）。
    for s in seconds().filter(|s| *s < u64::MAX / SECOND) {
        for ms in [s * SECOND, (s * SECOND).saturating_sub(1), s * SECOND + 1] {
            assert_eq!(retry_status::countdown(ms), old::countdown(ms), "{ms}");
        }
    }
}

#[test]
fn hours_minutes_and_seconds_are_unchanged() {
    for s in seconds() {
        assert_eq!(i18n::elapsed_plain(s), old::elapsed_plain(s), "{s}");
        assert_eq!(i18n::elapsed(s), old::elapsed(s), "{s}");
    }
}

#[test]
fn timestamps_are_unchanged() {
    const OFFSETS: [i32; 6] = [-720, -300, 0, 330, 345, 840];
    let base = 1_793_335_920_000_u64;
    for step in 0..=(2 * DAY_SECONDS) {
        let at = base + step * 997;
        for offset in OFFSETS {
            assert_eq!(
                session_rows::timestamp(at, offset),
                old::timestamp(at, offset),
                "{at} {offset}"
            );
            assert_eq!(
                export::default_name(None, at, offset),
                format!("conversation-{}.txt", old::export_timestamp(at, offset)),
                "{at} {offset}"
            );
        }
    }
    // 纪元附近：偏移是负的时本地时间在纪元之前，两份都钉到 0。
    for at in [0, 1, 59_999, 60_000, 43_200_000] {
        for offset in OFFSETS {
            assert_eq!(
                session_rows::timestamp(at, offset),
                old::timestamp(at, offset)
            );
            assert_eq!(
                export::default_name(None, at, offset),
                format!("conversation-{}.txt", old::export_timestamp(at, offset))
            );
        }
    }
}

#[test]
fn thousands_are_unchanged() {
    for count in (0..=2_000_000_u64)
        .step_by(7)
        .chain([999, 1000, 1049, 1050, 1051, 9_999_999])
    {
        assert_eq!(goal::tokens(count), old::tokens(count), "{count}");
        assert_eq!(
            goal::tokens(count),
            old::tokens_label(usize::try_from(count).unwrap()),
            "{count}"
        );
    }
}

#[test]
fn tenths_are_unchanged_for_non_negative_values() {
    let context = |value: f64| format!("{:.1}", (value * 10.0).round() / 10.0);
    let footer = |value: f64| format!("{:.1}", (value * 10.0).round().max(0.0) / 10.0);
    for step in 0..=200_000_u32 {
        let value = f64::from(step) * 0.000_73;
        assert_eq!(tenths(value), context(value), "{value}");
        assert_eq!(tenths(value), footer(value), "{value}");
    }
    for value in [0.05, 0.15, 0.25, 99.95, 100.0, 1e9] {
        assert_eq!(tenths(value), context(value), "{value}");
    }
}

#[test]
fn day_numbers_match_the_civil_calendar() {
    // 1600-01-01 到 2400-12-31 逐日。
    let (first, last) = (
        civil::days_from_civil(1600, 1, 1),
        civil::days_from_civil(2400, 12, 31),
    );
    for day in first..=last {
        let (year, month, date) = civil::civil_from_days(day);
        let text = format!("{year:04}-{month:02}-{date:02}");
        assert_eq!(usage_stats::day_number(&text), Some(day), "{text}");
        assert_eq!(old::day_number(&text), Some(day), "{text}");
        // 周日为 0（1970-01-01 是周四）。
        assert_eq!(
            i64::from(civil::weekday(day)),
            (day + 4).rem_euclid(7),
            "{text}"
        );
    }
    for text in ["", "2026", "2026-10", "2026-x-01", "2026-10-y"] {
        assert_eq!(
            usage_stats::day_number(text),
            old::day_number(text),
            "{text}"
        );
    }
}

#[test]
fn kitty_ids_are_unchanged() {
    for seed in [
        "",
        "a",
        "zcode",
        "0123456789abcdef",
        "通知",
        "x".repeat(300).as_str(),
    ] {
        assert_eq!(notify::id(seed), old::notify_id(seed), "{seed}");
    }
    for n in 0..5_000_u32 {
        let seed = format!("{n:x}-{}", n * 7919);
        assert_eq!(notify::id(&seed), old::notify_id(&seed), "{seed}");
    }
}

#[test]
fn the_helpers_split_at_the_unit_boundaries() {
    assert_eq!(largest_unit(0), (0, Unit::Second));
    assert_eq!(largest_unit(59), (59, Unit::Second));
    assert_eq!(largest_unit(60), (1, Unit::Minute));
    assert_eq!(largest_unit(3_599), (59, Unit::Minute));
    assert_eq!(largest_unit(3_600), (1, Unit::Hour));
    assert_eq!(largest_unit(86_399), (23, Unit::Hour));
    assert_eq!(largest_unit(86_400), (1, Unit::Day));
    assert_eq!(largest_unit_text(90_500), "1m");
    assert_eq!(hours_minutes_seconds(90_061), (25, 1, 1));
    // 2026-10-30 04:52:07 UTC。
    assert_eq!(time_of_day(1_793_335_927_000), (4, 52, 7));
    assert_eq!(thousands(999), "999");
    assert_eq!(thousands(1_250), "1.2k");
}
