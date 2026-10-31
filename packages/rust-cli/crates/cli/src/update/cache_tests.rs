#![allow(
    clippy::disallowed_methods,
    reason = "测试：临时目录、读回写出的文件、列目录看有没有留下临时文件"
)]
use super::*;

const HOUR_MS: u64 = 60 * 60 * 1000;
/// 2026-10-20T08:00:00.000Z。
const NOW_MS: u64 = 1_792_483_200_000;

fn info(latest: &str, at_ms: i64, dismissed: Option<&str>) -> VersionInfo {
    VersionInfo {
        latest_version: latest.to_owned(),
        last_checked_at_ms: at_ms,
        dismissed_version: dismissed.map(str::to_owned),
    }
}

#[test]
fn the_file_is_one_json_line_with_three_fields() {
    let text = info("0.1.47", NOW_MS as i64, None).render();
    assert_eq!(
        text,
        "{\"latest_version\":\"0.1.47\",\"last_checked_at\":\"2026-10-20T08:00:00.000Z\",\"dismissed_version\":null}\n"
    );
    let dismissed = info("0.1.47", NOW_MS as i64, Some("0.1.47")).render();
    assert!(dismissed.contains("\"dismissed_version\":\"0.1.47\""));
    assert_eq!(
        VersionInfo::parse(dismissed.as_bytes()),
        Some(info("0.1.47", NOW_MS as i64, Some("0.1.47")))
    );
}

#[test]
fn bad_files_are_treated_as_missing() {
    for bad in [
        "",
        "not json",
        "[]",
        "{\"last_checked_at\":\"2026-10-20T08:00:00Z\"}",
        "{\"latest_version\":\"0.1.47\"}",
        "{\"latest_version\":1,\"last_checked_at\":\"2026-10-20T08:00:00Z\"}",
        "{\"latest_version\":\"0.1.47\",\"last_checked_at\":\"yesterday\"}",
        "{\"latest_version\":\"0.1.47\",\"last_checked_at\":1792483200000}",
    ] {
        assert_eq!(VersionInfo::parse(bad.as_bytes()), None, "{bad}");
    }
    // `dismissed_version` 缺失或不是字符串：当作没有。
    let missing = "{\"latest_version\":\"0.1.47\",\"last_checked_at\":\"2026-10-20T08:00:00Z\"}";
    assert_eq!(
        VersionInfo::parse(missing.as_bytes()),
        Some(info("0.1.47", NOW_MS as i64, None))
    );
}

#[test]
fn timestamps_are_rfc3339() {
    assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(
        parse_rfc3339("2026-10-20T08:00:00.000Z"),
        Some(NOW_MS as i64)
    );
    // chrono 写的纳秒与带偏移的写法也认（别的写入方用 chrono 的 `DateTime<Utc>`）。
    assert_eq!(
        parse_rfc3339("2026-10-20T08:00:00.123456789Z"),
        Some(NOW_MS as i64 + 123)
    );
    assert_eq!(
        parse_rfc3339("2026-10-20T16:00:00+08:00"),
        Some(NOW_MS as i64)
    );
    assert_eq!(
        parse_rfc3339("2026-10-20t02:30:00.5-05:30z"),
        None,
        "two zones"
    );
    assert_eq!(
        parse_rfc3339("2026-10-20t02:30:00.5-05:30"),
        Some(NOW_MS as i64 + 500)
    );
    assert_eq!(
        parse_rfc3339("2024-02-29T00:00:00Z"),
        Some(1_709_164_800_000)
    );
    for bad in [
        "2026-10-20",
        "2026-10-20 08:00:00Z",
        "2026-13-01T00:00:00Z",
        "2026-02-30T00:00:00Z",
        "2025-02-29T00:00:00Z",
        "2026-10-20T24:00:00Z",
        "2026-10-20T08:00:00",
        "2026-10-20T08:00:00.Z",
        "2026-10-20T08:00:00+0800",
        "+026-10-20T08:00:00Z",
    ] {
        assert_eq!(parse_rfc3339(bad), None, "{bad}");
    }
}

#[test]
fn the_cache_is_refreshed_after_twenty_hours() {
    assert!(VersionInfo::stale(None, NOW_MS));
    let checked = |hours_ago: u64| info("0.1.47", (NOW_MS - hours_ago * HOUR_MS) as i64, None);
    assert!(!VersionInfo::stale(Some(&checked(1)), NOW_MS));
    // 正好 20 小时不刷新（条件是 `last_checked_at < now - 20h`），多 1 毫秒就刷新。
    assert!(!VersionInfo::stale(Some(&checked(20)), NOW_MS));
    let older = info("0.1.47", (NOW_MS - 20 * HOUR_MS) as i64 - 1, None);
    assert!(VersionInfo::stale(Some(&older), NOW_MS));
    assert!(VersionInfo::stale(Some(&info("0.1.47", 0, None)), NOW_MS));
    // 时钟回拨（缓存在未来）：不刷新。
    assert!(!VersionInfo::stale(
        Some(&info("0.1.47", NOW_MS as i64 + HOUR_MS as i64, None)),
        NOW_MS
    ));
}

#[tokio::test]
async fn refreshing_keeps_the_dismissed_version_and_writes_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rust-cli").join("version.json");
    // 目录不存在时创建。
    record_latest(&path, "0.1.47", NOW_MS).await.unwrap();
    assert_eq!(read(&path).await, Some(info("0.1.47", NOW_MS as i64, None)));
    dismiss(&path, "0.1.47").await.unwrap();
    record_latest(&path, "0.1.48", NOW_MS + HOUR_MS)
        .await
        .unwrap();
    assert_eq!(
        read(&path).await,
        Some(info("0.1.48", (NOW_MS + HOUR_MS) as i64, Some("0.1.47")))
    );
    // 原子写：目录里只有目标文件，没有留下临时文件。
    let names: Vec<String> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["version.json"]);
}

#[tokio::test]
async fn dismissing_without_a_cache_forces_the_next_check() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("version.json");
    dismiss(&path, "0.1.47").await.unwrap();
    let written = read(&path).await.unwrap();
    assert_eq!(written, info("0.1.47", 0, Some("0.1.47")));
    assert!(VersionInfo::stale(Some(&written), NOW_MS));
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("\"last_checked_at\":\"1970-01-01T00:00:00.000Z\"")
    );
    // 坏文件同样当作没有。
    std::fs::write(&path, "garbage").unwrap();
    assert_eq!(read(&path).await, None);
    dismiss(&path, "0.1.48").await.unwrap();
    assert_eq!(read(&path).await, Some(info("0.1.48", 0, Some("0.1.48"))));
}

// 收拢前的 RFC 3339 解析（逐字照抄，含它的月长与公历换算），核对改用 base 常量之后的结果相同。
fn old_parse_rfc3339(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    let number = |range: std::ops::Range<usize>| -> Option<i64> {
        let part = text.get(range)?;
        part.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    if bytes.len() < 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !matches!(bytes[10], b'T' | b't')
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1..=12).contains(&month)
        || !(1..=days_in_month(year, month)).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let mut rest = &text[19..];
    let mut millis = 0;
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        let padded = format!("{:0<3}", &fraction[..digits.min(3)]);
        millis = padded.parse::<i64>().ok()?;
        rest = &fraction[digits..];
    }
    let offset_minutes = match rest.as_bytes() {
        [b'Z' | b'z'] => 0,
        [sign @ (b'+' | b'-'), h1, h2, b':', m1, m2] => {
            let digit = |b: &u8| b.is_ascii_digit().then(|| i64::from(b - b'0'));
            let hours = digit(h1)? * 10 + digit(h2)?;
            let minutes = digit(m1)? * 10 + digit(m2)?;
            if hours > 23 || minutes > 59 {
                return None;
            }
            let total = hours * 60 + minutes;
            if *sign == b'-' { -total } else { total }
        }
        _ => return None,
    };
    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset_minutes * 60;
    Some(seconds * 1000 + millis)
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// 公历日期 → 自 1970-01-01 起的天数（Howard Hinnant 的 days_from_civil）。
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[test]
fn rfc3339_matches_the_old_parser() {
    let mut samples: Vec<String> = Vec::new();
    for days in (rust_cli_base::civil::days_from_civil(1600, 1, 1)
        ..=rust_cli_base::civil::days_from_civil(2400, 12, 31))
        .step_by(7)
    {
        let date = rust_cli_base::civil::format_date(days);
        samples.push(format!("{date}T00:00:00Z"));
        samples.push(format!("{date}t23:59:60.5+14:59"));
        samples.push(format!("{date}T12:30:45.123456-05:30"));
    }
    for odd in [
        "2026-02-29T00:00:00Z",
        "2024-02-30T00:00:00Z",
        "2026-04-31T00:00:00Z",
        "2026-00-10T00:00:00Z",
        "2026-12-32T00:00:00Z",
        "2026-10-20T23:60:00Z",
        "2026-10-20T23:59:61Z",
        "2026-10-20T08:00:00+24:00",
        "2026-10-20T08:00:00-23:60",
        "2026-10-20T08:00:00.Z",
        "2026-10-20T08:00:00",
        "2026-10-20T08:00:0Z",
        "2026-10-20X08:00:00Z",
        "2026-10-20T08-00:00Z",
        "",
        "1970-01-01T00:00:00z",
    ] {
        samples.push(odd.to_owned());
    }
    for text in &samples {
        assert_eq!(parse_rfc3339(text), old_parse_rfc3339(text), "{text}");
    }
}
