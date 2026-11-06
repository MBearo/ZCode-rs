//! 日期的换算与 App 的 `new Date(...)` 相同（数值对照 Node 22 的 `Date.parse`）。

use super::*;

const EAST_8: &(dyn Fn(u64) -> i32 + Sync) = &|_| 480;

#[test]
fn days_and_dates_round_trip() {
    assert_eq!(day_of_date("1970-01-01"), Some(0));
    assert_eq!(day_of_date("2026-10-30"), Some(20_756));
    assert_eq!(date_of_day(20_756), "2026-10-30");
    assert_eq!(day_of_date("2024-02-29"), Some(19_782));
    // V8 让超出当月的日顺延：Date.parse("2026-02-29") 是 3 月 1 日。
    assert_eq!(day_of_date("2026-02-29"), day_of_date("2026-03-01"));
    assert_eq!(day_of_date("2026-02-32"), None);
    assert_eq!(day_of_date("2026-13-01"), None);
    assert_eq!(day_of_date("2026-1-01"), None);
    // 2026-10-25 是周日。
    assert_eq!(weekday(day_of_date("2026-10-25").unwrap()), 0);
    assert_eq!(weekday(day_of_date("2026-10-30").unwrap()), 5);
}

#[test]
fn times_parse_like_the_app() {
    // Date.parse("2026-11-30") === 1795996800000（只有日期：UTC）
    assert_eq!(parse_time("2026-11-30", EAST_8), Some(1_795_996_800_000));
    // 不带时区的时刻按本地（东八区）：Date.parse("2026-11-30T23:59:59") 在 Asia/Shanghai 是 1796054399000。
    assert_eq!(
        parse_time("2026-11-30 23:59:59", EAST_8),
        Some(1_796_054_399_000)
    );
    assert_eq!(
        parse_time(" 2026-11-30T23:59:59 ", EAST_8),
        Some(1_796_054_399_000)
    );
    assert_eq!(
        parse_time("2026-11-30T15:59:59.000Z", EAST_8),
        Some(1_796_054_399_000)
    );
    assert_eq!(
        parse_time("2026-11-30T23:59:59+08:00", EAST_8),
        Some(1_796_054_399_000)
    );
    assert_eq!(
        parse_time("2026-11-30T23:59", EAST_8),
        Some(1_796_054_340_000)
    );
    assert_eq!(parse_time("soon", EAST_8), None);
    assert_eq!(parse_time("2026-11-31", EAST_8), Some(1_796_083_200_000));
    assert_eq!(
        parse_time("2026-11-30 24:00:00", EAST_8),
        Some(1_796_054_400_000)
    );
    assert_eq!(parse_time("2026-11-30 25:00:00", EAST_8), None);
}

#[test]
fn today_is_the_local_date() {
    struct At(u64, i32);
    impl Clock for At {
        fn now_ms(&self) -> u64 {
            self.0
        }
        fn time_zone(&self) -> String {
            "X".to_owned()
        }
        fn utc_offset_minutes(&self, _at_ms: u64) -> i32 {
            self.1
        }
    }
    // 2026-10-29T20:00:00Z：UTC 还是 29 号，东八区已是 30 号。
    let at = 1_793_304_000_000;
    assert_eq!(date_of_day(today(&At(at, 0))), "2026-10-29");
    assert_eq!(date_of_day(today(&At(at, 480))), "2026-10-30");
    assert_eq!(date_of_day(today(&At(at, -300))), "2026-10-29");
}
