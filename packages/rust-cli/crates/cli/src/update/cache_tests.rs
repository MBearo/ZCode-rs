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
fn the_file_is_one_json_line_with_the_codex_fields() {
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
    // `dismissed_version` 缺失或不是字符串：当作没有（Codex `#[serde(default)]`）。
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
    // chrono 写的纳秒与带偏移的写法也认（Codex 的 `DateTime<Utc>`）。
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
    // 正好 20 小时不刷新（Codex：`last_checked_at < now - 20h`），多 1 毫秒就刷新。
    assert!(!VersionInfo::stale(Some(&checked(20)), NOW_MS));
    let older = info("0.1.47", (NOW_MS - 20 * HOUR_MS) as i64 - 1, None);
    assert!(VersionInfo::stale(Some(&older), NOW_MS));
    assert!(VersionInfo::stale(Some(&info("0.1.47", 0, None)), NOW_MS));
    // 时钟回拨（缓存在未来）：不刷新，与 Codex 相同。
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
