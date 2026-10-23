use std::sync::Arc;

use rust_cli_kernel_api::clock::Clock;

use super::*;

#[test]
fn local_dates_shift_by_the_offset() {
    // 2025-10-09T08:53:20Z
    let at = 1_760_000_000_000;
    assert_eq!(local_date(at, 0), "2025-10-09");
    assert_eq!(local_date(at, 16 * 60), "2025-10-10");
    assert_eq!(local_date(at, -9 * 60), "2025-10-08");
}

/// 修复（R2-6）：以前 `SystemClock` 只存时区名，`today()` 与 `utc_offset_minutes()` 一律按 UTC。
#[test]
fn the_system_clock_follows_its_time_zone() {
    let east = SystemClock::new(Arc::new(FixedOffset::new("Etc/GMT-14", 14 * 60)));
    let west = SystemClock::new(Arc::new(FixedOffset::new("Etc/GMT+12", -12 * 60)));
    // 相差 26 小时：两边的日期总是不同。
    assert_ne!(east.today(), west.today());
    assert_eq!(east.utc_offset_minutes(0), 14 * 60);
    assert_eq!(west.utc_offset_minutes(1_760_000_000_000), -12 * 60);
    assert_eq!(east.time_zone(), "Etc/GMT-14");
    let utc = SystemClock::utc();
    assert_eq!(utc.time_zone(), "UTC");
    assert_eq!(utc.utc_offset_minutes(0), 0);
}
