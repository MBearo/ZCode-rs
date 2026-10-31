use super::*;

#[test]
fn derived_units_have_the_familiar_values() {
    assert_eq!(MS_PER_MINUTE, 60_000);
    assert_eq!(MS_PER_HOUR, 3_600_000);
    assert_eq!(MS_PER_DAY, 86_400_000);
    assert_eq!(SECONDS_PER_HOUR, 3_600);
    assert_eq!(SECONDS_PER_DAY, 86_400);
    assert_eq!(MINUTES_PER_DAY, 1_440);
    assert_eq!(MICROS_PER_MS, 1_000);
    assert_eq!(MIB, 1_048_576);
    assert_eq!(FILETIME_EPOCH_OFFSET_MS, 11_644_473_600_000);
    assert_eq!(MS_PER_SECOND_F64, MS_PER_SECOND as f64);
}

#[test]
fn milliseconds_round_half_up_to_seconds() {
    assert_eq!(round_ms_to_seconds(0), 0);
    assert_eq!(round_ms_to_seconds(499), 0);
    assert_eq!(round_ms_to_seconds(500), 1);
    assert_eq!(round_ms_to_seconds(1_499), 1);
    assert_eq!(round_ms_to_seconds(120_000), 120);
    assert_eq!(round_ms_to_seconds(u64::MAX), u64::MAX / 1_000);
}

/// 收拢前 agents 同伴列表的写法（逐字照抄）：只取最大的单位。
fn old_ago(ms: u64) -> String {
    if ms < 60_000 {
        return format!("{}s", ms / 1000);
    }
    let mut days = ms / 86_400_000;
    let mut hours = (ms % 86_400_000) / 3_600_000;
    let mut minutes = (ms % 3_600_000) / 60_000;
    let seconds = ((ms % 60_000) as f64 / 1000.0).round() as u64;
    if seconds == 60 {
        minutes += 1;
    }
    if minutes == 60 {
        minutes = 0;
        hours += 1;
    }
    if hours == 24 {
        hours = 0;
        days += 1;
    }
    if days > 0 {
        format!("{days}d")
    } else if hours > 0 {
        format!("{hours}h")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        format!("{seconds}s")
    }
}

/// 收拢前 kernel-api 时长写法的拆分部分（逐字照抄）。
fn old_parts(ms: u64) -> (u64, u64, u64, u64) {
    let (mut days, mut hours, mut minutes) = (
        ms / 86_400_000,
        (ms % 86_400_000) / 3_600_000,
        (ms % 3_600_000) / 60_000,
    );
    let mut seconds = (ms % 60_000 + 500) / 1000;
    if seconds == 60 {
        seconds = 0;
        minutes += 1;
    }
    if minutes == 60 {
        minutes = 0;
        hours += 1;
    }
    if hours == 24 {
        hours = 0;
        days += 1;
    }
    (days, hours, minutes, seconds)
}

fn new_ago(ms: u64) -> String {
    if ms < MS_PER_MINUTE {
        return format!("{}s", ms / MS_PER_SECOND);
    }
    let parts = split_ms(ms);
    if parts.days > 0 {
        format!("{}d", parts.days)
    } else if parts.hours > 0 {
        format!("{}h", parts.hours)
    } else if parts.minutes > 0 {
        format!("{}m", parts.minutes)
    } else {
        format!("{}s", parts.seconds)
    }
}

/// 0–10 天：前 3 分钟逐毫秒，之后按 499 ms 的步长（落在各种余数上），再加每个分、时、天边界前后 1 秒逐毫秒。
#[test]
fn split_matches_both_old_copies_for_ten_days() {
    let check = |ms: u64| {
        let parts = split_ms(ms);
        assert_eq!(
            (parts.days, parts.hours, parts.minutes, parts.seconds),
            old_parts(ms),
            "{ms}"
        );
        assert_eq!(new_ago(ms), old_ago(ms), "{ms}");
    };
    (0..180_000).for_each(check);
    (180_000..10 * MS_PER_DAY).step_by(499).for_each(check);
    for boundary in (MS_PER_MINUTE..=10 * MS_PER_DAY).step_by(MS_PER_MINUTE as usize) {
        (boundary - 1_000..boundary + 1_000).for_each(check);
    }
}
