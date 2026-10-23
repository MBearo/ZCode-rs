//! 本机的系统时钟（docs/specs/rust-cli/README.md §4 的“时间”类）：全工程业务时间的唯一来源。入口建一份，
//! 内核（`Ports.clock`）、node-store 的记录、model、env-local 与 TUI 都用它（F7，审查 R2-11；以前在 runtime，
//! node-store 另有一份 `RecordSource`）。
//!
//! 时区（docs/specs/rust-cli/time-zone.md §1，F8，审查 R2-6）：入口读系统时区后实现 [`TimeZone`] 交给时钟；
//! `today()` 与 `utc_offset_minutes()` 按它给本地日期与每个时刻的偏移。

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rust_cli_kernel_api::clock::{Clock, civil_date};

/// 时区规则：入口读系统时区后实现，内核只经 [`Clock`] 用它。
pub trait TimeZone: Send + Sync {
    /// 名字，只作信息（IANA 名，Windows 上是系统的时区键名）。
    fn name(&self) -> String;

    /// `at_ms` 这一刻相对 UTC 的偏移（分钟，东正西负）。
    fn offset_minutes(&self, at_ms: u64) -> i32;
}

/// 固定偏移的时区：UTC（`TZ` 是 UTC 写法时）与测试。
pub struct FixedOffset {
    name: String,
    minutes: i32,
}

impl FixedOffset {
    pub fn new(name: impl Into<String>, minutes: i32) -> Self {
        Self {
            name: name.into(),
            minutes,
        }
    }

    pub fn utc() -> Self {
        Self::new("UTC", 0)
    }
}

impl TimeZone for FixedOffset {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn offset_minutes(&self, _at_ms: u64) -> i32 {
        self.minutes
    }
}

/// 系统时钟。时区由入口 crate 解析后传入（库内不读进程环境）。
pub struct SystemClock {
    zone: Arc<dyn TimeZone>,
}

impl SystemClock {
    pub fn new(zone: Arc<dyn TimeZone>) -> Self {
        Self { zone }
    }

    /// 按 UTC 的时钟（测试与不关心时区的地方）。
    pub fn utc() -> Self {
        Self::new(Arc::new(FixedOffset::utc()))
    }
}

/// `utc_ms` 加上偏移之后的日期 `YYYY-MM-DD`。
pub(crate) fn local_date(utc_ms: u64, offset_minutes: i32) -> String {
    let utc = i64::try_from(utc_ms).unwrap_or(i64::MAX);
    let local = utc.saturating_add(i64::from(offset_minutes) * 60_000);
    civil_date(u64::try_from(local).unwrap_or(0))
}

impl Clock for SystemClock {
    #[expect(
        clippy::disallowed_methods,
        reason = "SystemClock 是业务时间的唯一来源（README §4 的时间类入口）"
    )]
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0)
    }

    fn time_zone(&self) -> String {
        self.zone.name()
    }

    /// 修复（R2-6）：以前用 kernel-api 的缺省实现，一律按 UTC；现在按入口给的时区、现在这一刻的偏移
    /// 算本地日期。
    fn today(&self) -> String {
        let now = self.now_ms();
        local_date(now, self.zone.offset_minutes(now))
    }

    fn utc_offset_minutes(&self, at_ms: u64) -> i32 {
        self.zone.offset_minutes(at_ms)
    }
}

#[cfg(test)]
#[path = "clock_tests.rs"]
mod tests;
