//! `version.json`（docs/specs/rust-cli/update.md §4，Codex `updates_cache.rs`）：最近一次核对的最新版本、核对时刻与
//! 不再提醒的版本。读经 fs 的 `read_optional`，写经 fs 的原子写；位置由 paths 给出（`StoragePaths::update_cache`）。

use std::io;
use std::path::Path;

use rust_cli_base::js::iso_timestamp;
use rust_cli_fs::{FileMode, ParentDir, TempName, WriteOptions};
use serde_json::{Value, json};

/// 缓存过了多久要刷新（Codex：20 小时）。
pub(crate) const REFRESH_AFTER_MS: i64 = 20 * 60 * 60 * 1000;

/// 文件的内容（字段同 Codex `VersionInfo`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VersionInfo {
    pub(crate) latest_version: String,
    /// Unix 毫秒（RFC 3339 读进来的值，可以早于 1970）。
    pub(crate) last_checked_at_ms: i64,
    pub(crate) dismissed_version: Option<String>,
}

impl VersionInfo {
    /// 缓存缺失或早于“现在 − 20 小时”时要刷新（Codex `get_upgrade_version`）。
    pub(crate) fn stale(info: Option<&Self>, now_ms: u64) -> bool {
        let now = i64::try_from(now_ms).unwrap_or(i64::MAX);
        info.is_none_or(|info| info.last_checked_at_ms < now.saturating_sub(REFRESH_AFTER_MS))
    }

    /// 一行 JSON 加换行（字段顺序同 Codex）。
    pub(crate) fn render(&self) -> String {
        let at = u64::try_from(self.last_checked_at_ms).unwrap_or(0);
        let value = json!({
            "latest_version": self.latest_version,
            "last_checked_at": iso_timestamp(at),
            "dismissed_version": self.dismissed_version,
        });
        format!("{value}\n")
    }

    /// 解析文件内容；前两个字段缺失、类型不对或时刻不是 RFC 3339 时为 None。`dismissed_version` 缺失或不是字符串
    /// 时当作没有（Codex `#[serde(default)]`）。
    pub(crate) fn parse(bytes: &[u8]) -> Option<Self> {
        let value: Value = serde_json::from_slice(bytes).ok()?;
        let latest_version = value.get("latest_version")?.as_str()?.to_owned();
        let last_checked_at_ms = parse_rfc3339(value.get("last_checked_at")?.as_str()?)?;
        let dismissed_version = value
            .get("dismissed_version")
            .and_then(Value::as_str)
            .map(str::to_owned);
        Some(Self {
            latest_version,
            last_checked_at_ms,
            dismissed_version,
        })
    }
}

fn options() -> WriteOptions {
    WriteOptions {
        mode: FileMode::Default,
        temp_name: TempName::Hidden,
        parent: ParentDir::Create,
        rename_retry: &[],
    }
}

/// 读缓存；文件不存在、读不出或内容不对时为 None（读错误记日志）。
pub(crate) async fn read(path: &Path) -> Option<VersionInfo> {
    match rust_cli_fs::read_optional(path).await {
        Ok(Some(bytes)) => VersionInfo::parse(&bytes),
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(%error, "update: the version cache could not be read");
            None
        }
    }
}

async fn write(path: &Path, info: &VersionInfo) -> io::Result<()> {
    rust_cli_fs::atomic_write(path, info.render().as_bytes(), options()).await
}

/// 刷新得到的最新版本：先重读文件，保留其中的 `dismissed_version`（Codex `check_for_update`）。
pub(crate) async fn record_latest(path: &Path, latest: &str, now_ms: u64) -> io::Result<()> {
    let previous = read(path).await;
    let info = VersionInfo {
        latest_version: latest.to_owned(),
        last_checked_at_ms: i64::try_from(now_ms).unwrap_or(i64::MAX),
        dismissed_version: previous.and_then(|info| info.dismissed_version),
    };
    write(path, &info).await
}

/// 不再提醒这个版本（Codex `dismiss_version`）：没有文件或坏文件时以 1970 年为核对时刻新建，下次启动会重新检查。
pub(crate) async fn dismiss(path: &Path, version: &str) -> io::Result<()> {
    let mut info = read(path).await.unwrap_or_else(|| VersionInfo {
        latest_version: version.to_owned(),
        last_checked_at_ms: 0,
        dismissed_version: None,
    });
    info.dismissed_version = Some(version.to_owned());
    write(path, &info).await
}

/// RFC 3339 → Unix 毫秒：`YYYY-MM-DDTHH:MM:SS[.小数](Z|±HH:MM)`（`T` / `Z` 大小写都认）。小数只取到毫秒。
pub(crate) fn parse_rfc3339(text: &str) -> Option<i64> {
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

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
