use super::*;

/// 逐日往前走的公历（只用闰年规则与月长），给算法当参照。
struct Walk {
    year: i64,
    month: u32,
    day: u32,
}

impl Walk {
    fn days_in_month(&self) -> u32 {
        let leap = (self.year % 4 == 0 && self.year % 100 != 0) || self.year % 400 == 0;
        match self.month {
            2 if leap => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        }
    }

    fn next(&mut self) {
        if self.day < self.days_in_month() {
            self.day += 1;
        } else if self.month < 12 {
            self.month += 1;
            self.day = 1;
        } else {
            self.year += 1;
            self.month = 1;
            self.day = 1;
        }
    }
}

/// 收拢前 base `js::date` 与 tools `cron_expr` 里的实现（逐字照抄），核对新函数与它们逐日相同。
fn old_civil(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

fn old_days_from(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let month = i64::from(month);
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[test]
fn every_day_from_1600_to_2400_matches_a_calendar_walk_and_the_old_code() {
    let mut walk = Walk {
        year: 1600,
        month: 1,
        day: 1,
    };
    let mut days = old_days_from(1600, 1, 1);
    let mut weekday_now = weekday(days);
    while walk.year <= 2400 {
        let date = (walk.year, walk.month, walk.day);
        assert_eq!(
            days_from_civil(walk.year, walk.month, walk.day),
            days,
            "{date:?}"
        );
        assert_eq!(
            old_days_from(walk.year, walk.month, walk.day),
            days,
            "{date:?}"
        );
        assert_eq!(civil_from_days(days), date);
        assert_eq!(old_civil(days), date);
        assert_eq!(weekday(days), weekday_now);
        walk.next();
        days += 1;
        weekday_now = (weekday_now + 1) % 7;
    }
}

#[test]
fn known_dates() {
    assert_eq!(days_from_civil(1970, 1, 1), 0);
    assert_eq!(civil_from_days(0), (1970, 1, 1));
    assert_eq!(civil_from_days(-1), (1969, 12, 31));
    assert_eq!(days_from_civil(2000, 2, 29), 11_016);
    assert_eq!(civil_from_days(20_454), (2026, 1, 1));
    // 1970-01-01 是星期四，2026-10-30 是星期五。
    assert_eq!(weekday(0), 4);
    assert_eq!(weekday(days_from_civil(2026, 10, 30)), 5);
    assert_eq!(weekday(-1), 3);
}

/// 收拢前 kernel-api `clock::civil_date` 的写法（逐字照抄）。
fn old_civil_date(now_ms: u64) -> String {
    let days = (now_ms / 86_400_000) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// 收拢前 cli 日志清理的 `parse_date`（逐字照抄）。
fn old_parse_date(date: &str) -> Option<i64> {
    let mut parts = date.split('-');
    let (year, month, day) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || year.len() != 4 || month.len() != 2 || day.len() != 2 {
        return None;
    }
    let (year, month, day): (i64, i64, i64) =
        (year.parse().ok()?, month.parse().ok()?, day.parse().ok()?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let shifted = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * shifted + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}

/// 收拢前 cli 更新缓存的 `days_in_month`（逐字照抄）。
fn old_days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// 收拢前 session 定时任务折叠的 `civil`（逐字照抄）：月份是 0 起的下标。
fn old_loop_civil(days: i64) -> (i64, usize, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, usize::try_from(month - 1).unwrap_or(0), day)
}

/// 收拢前 tools 定时任务的 `twelve_hour`（逐字照抄）。
fn old_twelve_hour(hour: u32) -> (u32, &'static str) {
    match hour {
        0 => (12, "AM"),
        _ if hour < 12 => (hour, "AM"),
        12 => (12, "PM"),
        _ => (hour - 12, "PM"),
    }
}

#[test]
fn date_text_helpers_match_the_old_copies_day_by_day() {
    let first = days_from_civil(1600, 1, 1);
    let last = days_from_civil(2400, 12, 31);
    for days in first..=last {
        let text = format_date(days);
        assert_eq!(parse_date(&text), Some(days), "{text}");
        assert_eq!(old_parse_date(&text), Some(days), "{text}");
        let (year, month, day) = civil_from_days(days);
        assert_eq!(
            old_loop_civil(days),
            (year, (month - 1) as usize, i64::from(day))
        );
        if days >= 0 {
            let ms = days as u64 * crate::units::MS_PER_DAY;
            for at in [ms, ms + crate::units::MS_PER_DAY - 1] {
                assert_eq!(format_date(days), old_civil_date(at), "{at}");
            }
        }
    }
    for year in 1600..=2400 {
        for month in JANUARY..=DECEMBER {
            assert_eq!(
                i64::from(days_in_month(year, month)),
                old_days_in_month(year, i64::from(month)),
                "{year}-{month}"
            );
        }
    }
    for hour in 0..24 {
        assert_eq!(twelve_hour(hour), old_twelve_hour(hour), "{hour}");
    }
}

#[test]
fn strict_dates_reject_what_the_old_parser_rejected() {
    for text in [
        "2026-1-05",
        "2026-01-5",
        "26-01-05",
        "2026-13-01",
        "2026-00-10",
        "2026-01-32",
        "2026-01-00",
        "2026-01-05-01",
        "2026/01/05",
        "",
        "abcd-ef-gh",
    ] {
        assert_eq!(parse_date(text), None, "{text}");
        assert_eq!(old_parse_date(text), None, "{text}");
    }
    // 不查月长：2 月 31 日按日数往后算（与以前相同）。
    assert_eq!(parse_date("2026-02-31"), old_parse_date("2026-02-31"));
    assert_eq!(parse_date("+999-+1-+5"), old_parse_date("+999-+1-+5"));
}
