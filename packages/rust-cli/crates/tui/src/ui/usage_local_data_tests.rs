//! 「This computer」标签页的数据：连续的天数与热力图的级别（kit `Activity` 的形状）。
use rust_cli_base::js::date::DAY_MS;
use rust_cli_kernel_api::metering::{ActivityDay, ActivityRange, ActivityStats};

use super::{WEEKS, activity, day_number, streaks};

fn day(date: &str, messages: u64) -> ActivityDay {
    ActivityDay {
        date: date.into(),
        messages,
    }
}

/// 今天是 2026-10-18（周日）；四个活跃日，其中 10-17、10-18 连续。
fn stats() -> ActivityStats {
    let first = day_number("2026-10-08").unwrap() as u64 * DAY_MS;
    ActivityStats {
        range: Some(ActivityRange::All),
        today: "2026-10-18".into(),
        days: vec![
            day("2026-10-08", 3),
            day("2026-10-12", 9),
            day("2026-10-17", 1),
            day("2026-10-18", 2),
        ],
        sessions: 2,
        first_session_ms: Some(first),
        last_session_ms: Some(first + 10 * DAY_MS),
        ..ActivityStats::default()
    }
}

#[test]
fn streaks_count_back_from_today_and_find_the_longest_run() {
    assert_eq!(streaks(&stats()), (2, 2));
    // 今天没有消息：当前连续是 0。
    let mut quiet = stats();
    quiet.today = "2026-10-19".into();
    assert_eq!(streaks(&quiet), (0, 2));
    assert_eq!(streaks(&ActivityStats::default()), (0, 0));
}

#[test]
fn the_heat_map_ends_on_today_in_quartile_levels() {
    let a = activity(&stats());
    assert_eq!((a.total, a.best_day, a.streak), (15, 9, 2));
    assert_eq!(a.levels.len(), WEEKS);
    // 第一周从周日起；今天（周日）是最后一周的第一天，之后的日子是 -1。
    assert_eq!(day_number(&a.start).map(|d| (d + 4).rem_euclid(7)), Some(0));
    let last = &a.levels[WEEKS - 1];
    // 分位是 [1, 2, 3, 9] 的 p25 = 2、p50 = 3、p75 = 9：今天 2 条是第 2 级。
    assert_eq!(last[0], 2);
    assert!(last[1..].iter().all(|l| *l == -1), "{last:?}");
    // 10-17（周六）1 条：第 1 级；10-12（周一）9 条：第 4 级；没有消息的日子 0。
    let before = &a.levels[WEEKS - 2];
    assert_eq!((before[6], before[1], before[0]), (1, 4, 0));
    // 没有活动：没有级别以上的格子。
    let none = activity(&ActivityStats {
        today: "2026-10-18".into(),
        ..ActivityStats::default()
    });
    assert!(none.levels.iter().flatten().all(|l| *l <= 0));
}
