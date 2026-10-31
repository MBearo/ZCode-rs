//! 期望值在 TZ=UTC 下算出（起点 2026-10-17T10:07:30Z）。

use super::*;

const FROM: u64 = 1_792_231_650_000;

fn utc(_at: u64) -> i32 {
    0
}

#[test]
fn next_run_matches_the_expected_times() {
    for (expr, next) in [
        ("*/5 * * * *", Some(1_792_231_800_000)),
        ("30 14 28 2 *", Some(1_803_825_000_000)),
        ("0 9 * * 1-5", Some(1_792_400_400_000)),
        ("7 * * * *", Some(1_792_235_220_000)),
        ("0 */2 * * *", Some(1_792_238_400_000)),
        ("15 10 17 10 *", Some(1_792_232_100_000)),
        ("0 0 1 * 0", Some(1_792_281_600_000)),
        ("* * * * *", Some(1_792_231_680_000)),
        ("30 2 31 2 *", None),
        ("5-7/2 1 * * 7", Some(1_792_285_500_000)),
    ] {
        assert_eq!(next_after(expr, FROM, &utc), next, "{expr}");
    }
}

#[test]
fn invalid_expressions_are_rejected() {
    for expr in [
        "* * * *",
        "60 * * * *",
        "* 24 * * *",
        "* * 0 * *",
        "* * * 13 *",
        "* * * * 8",
        "*/0 * * * *",
        "5-3 * * * *",
        "a * * * *",
        "1,,2 * * * *",
    ] {
        assert!(parse(expr).is_none(), "{expr}");
    }
    assert!(parse(" 1,2  3 * * 7 ").is_some());
}

#[test]
fn jitter_matches_the_expected_times() {
    let cases: [(&str, &str, u64, u64); 12] = [
        (
            "*/5 * * * *",
            "1a2b3c4d",
            1_792_231_935_000,
            1_792_231_800_000,
        ),
        (
            "0 9 * * *",
            "1a2b3c4d",
            1_792_315_800_000,
            1_792_313_990_800,
        ),
        (
            "*/10 * * * *",
            "1a2b3c4d",
            1_792_231_830_667,
            1_792_231_800_000,
        ),
        (
            "30 14 28 2 *",
            "1a2b3c4d",
            1_803_826_800_000,
            1_803_824_990_800,
        ),
        (
            "0 11 * * *",
            "1a2b3c4d",
            1_792_236_600_000,
            1_792_234_790_800,
        ),
        (
            "0 9 * * *",
            "ffffffff",
            1_792_315_800_000,
            1_792_313_910_000,
        ),
        (
            "*/10 * * * *",
            "ffffffff",
            1_792_232_100_000,
            1_792_231_800_000,
        ),
        (
            "0 11 * * *",
            "ffffffff",
            1_792_236_600_000,
            1_792_234_710_000,
        ),
        (
            "0 9 * * *",
            "00000000",
            1_792_314_000_000,
            1_792_314_000_000,
        ),
        (
            "*/10 * * * *",
            "00000000",
            1_792_231_800_000,
            1_792_231_800_000,
        ),
        ("0 11 * * *", "zz", 1_792_234_800_000, 1_792_234_800_000),
        ("*/5 * * * *", "zz", 1_792_231_935_000, 1_792_231_800_000),
    ];
    for (expr, id, recurring, one_shot) in cases {
        assert_eq!(
            recurring_next(expr, FROM, id, &utc),
            Some(recurring),
            "recurring {expr} {id}"
        );
        assert_eq!(
            one_shot_next(expr, FROM, id, &utc),
            Some(one_shot),
            "one-shot {expr} {id}"
        );
    }
}

#[test]
fn human_schedule_matches_the_expected_text() {
    for (expr, text) in [
        ("*/5 * * * *", "Every 5 minutes"),
        ("* * * * *", "Every minute"),
        ("*/1 * * * *", "Every minute"),
        ("0 * * * *", "Every hour"),
        ("5 * * * *", "Every hour at :05"),
        ("7 * * * *", "Every hour at :07"),
        ("0 */1 * * *", "Every hour"),
        ("0 */2 * * *", "Every 2 hours"),
        ("30 */3 * * *", "Every 3 hours at :30"),
        ("0 0 * * *", "Every day at 12:00 AM"),
        ("45 23 * * *", "Every day at 11:45 PM"),
        ("0 12 * * 6", "Every Saturday at 12:00 PM"),
        ("0 9 * * 7", "Every Sunday at 9:00 AM"),
        ("0 9 * * 1-5", "Weekdays at 9:00 AM"),
        ("30 14 28 2 *", "30 14 28 2 *"),
        ("1 2 3 4 5", "1 2 3 4 5"),
        ("bad", "bad"),
    ] {
        assert_eq!(human(expr), text, "{expr}");
    }
}

/// 偏移按时刻取：东八区的 `0 9 * * *` 是 UTC 01:00。
#[test]
fn local_time_follows_the_clock_offset() {
    let east8 = |_at: u64| 480;
    // 2026-10-17T10:07:30Z = 东八区 18:07:30，下一个 09:00 是 10-18 09:00 = 10-18T01:00Z。
    assert_eq!(
        next_after("0 9 * * *", FROM, &east8),
        Some(1_792_285_200_000)
    );
}

/// 公历日期与天数互转、星期（F122：换成 base 的 `civil` 前后逐日相同）：1600-01-01 到 2400-12-31 按闰年规则逐日走。
#[test]
fn calendar_conversions_match_a_day_by_day_walk_from_1600_to_2400() {
    let (mut year, mut month, mut day) = (1600_i64, 1_u32, 1_u32);
    let mut days = days_from(1600, 1, 1);
    // 1600-01-01 是星期六。
    let mut weekday = 6_u32;
    while year <= 2400 {
        assert_eq!(days_from(year, month, day), days, "{year}-{month}-{day}");
        assert_eq!(civil(days), (year, month, day));
        assert_eq!(weekday_of(days), weekday, "{year}-{month}-{day}");
        let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
        let length = match month {
            2 if leap => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        };
        if day < length {
            day += 1;
        } else if month < 12 {
            (month, day) = (month + 1, 1);
        } else {
            (year, month, day) = (year + 1, 1, 1);
        }
        days += 1;
        weekday = (weekday + 1) % 7;
    }
}
