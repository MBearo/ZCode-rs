//! `version.json`（docs/specs/rust-cli/update.md §4，Codex `updates_cache.rs`）：最近一次核对的最新版本、核对时刻与
//! 不再提醒的版本。读经 fs 的 `read_optional`，写经 fs 的原子写；位置由 paths 给出（`StoragePaths::update_cache`）。

use std::io;
use std::path::Path;

use rust_cli_base::civil::{self, iso};
use rust_cli_base::js::iso_timestamp;
use rust_cli_base::units;
use rust_cli_fs::{FileMode, ParentDir, TempName, WriteOptions};
use serde_json::{Value, json};

/// 缓存过了多久要刷新（20 小时，update.md §4）。
pub(crate) const REFRESH_AFTER_MS: i64 = 20 * units::MS_PER_HOUR as i64;
/// 秒的小数只取到毫秒：最多三位。
const MILLIS_DIGITS: usize = 3;
/// 时区偏移 `±HH:MM` 的十进制两位数。
const DECIMAL: i64 = units::DECIMAL_RADIX as i64;

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
    // 定长部分的布局、月长与公历换算只定义在 base 的 `civil`。
    let [first_dash, second_dash] = iso::DATE_DASHES;
    let [first_colon, second_colon] = iso::TIME_COLONS;
    if bytes.len() <= iso::DATETIME_LEN
        || bytes[first_dash] != b'-'
        || bytes[second_dash] != b'-'
        || !matches!(bytes[iso::TIME_MARKER], b'T' | b't')
        || bytes[first_colon] != b':'
        || bytes[second_colon] != b':'
    {
        return None;
    }
    let (year, month, day) = (number(iso::YEAR)?, number(iso::MONTH)?, number(iso::DAY)?);
    let (hour, minute, second) = (
        number(iso::HOUR)?,
        number(iso::MINUTE)?,
        number(iso::SECOND)?,
    );
    let (hours_per_day, minutes_per_hour, seconds_per_minute) = (
        units::HOURS_PER_DAY as i64,
        units::MINUTES_PER_HOUR as i64,
        units::SECONDS_PER_MINUTE as i64,
    );
    let month = u32::try_from(month)
        .ok()
        .filter(|month| (civil::JANUARY..=civil::DECEMBER).contains(month))?;
    // 秒可以是 60（闰秒）。
    if !(1..=i64::from(civil::days_in_month(year, month))).contains(&day)
        || hour >= hours_per_day
        || minute >= minutes_per_hour
        || second > seconds_per_minute
    {
        return None;
    }
    let mut rest = &text[iso::DATETIME_LEN..];
    let mut millis = 0;
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        let padded = format!("{:0<3}", &fraction[..digits.min(MILLIS_DIGITS)]);
        millis = padded.parse::<i64>().ok()?;
        rest = &fraction[digits..];
    }
    let offset_minutes = match rest.as_bytes() {
        [b'Z' | b'z'] => 0,
        [sign @ (b'+' | b'-'), h1, h2, b':', m1, m2] => {
            let digit = |b: &u8| b.is_ascii_digit().then(|| i64::from(b - b'0'));
            let hours = digit(h1)? * DECIMAL + digit(h2)?;
            let minutes = digit(m1)? * DECIMAL + digit(m2)?;
            if hours >= hours_per_day || minutes >= minutes_per_hour {
                return None;
            }
            let total = hours * minutes_per_hour + minutes;
            if *sign == b'-' { -total } else { total }
        }
        _ => return None,
    };
    let days = civil::days_from_civil(year, month, 1) + day - 1;
    let seconds = days * units::SECONDS_PER_DAY as i64
        + hour * units::SECONDS_PER_HOUR as i64
        + minute * seconds_per_minute
        + second
        - offset_minutes * seconds_per_minute;
    Some(seconds * units::MS_PER_SECOND as i64 + millis)
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
