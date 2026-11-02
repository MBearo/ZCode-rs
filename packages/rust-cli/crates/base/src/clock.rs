//! 本机的系统时钟（docs/specs/rust-cli/README.md §4 的“时间”类）：全工程业务时间的唯一来源。入口建一份，交给内核
//! （`Ports.clock`）、node-store 的记录、model、env-local、执行器的账本与 TUI（F7；F128 起在 base，审查 X9：以前在
//! node-store，执行器为了账本的时钟要引入会话库的 crate）。kernel-api 为它实现 `Clock` 端口。
//!
//! 时区（docs/specs/rust-cli/time-zone.md §1，F8）：入口读系统时区后实现 [`TimeZone`] 交给时钟；`today()` 与
//! `utc_offset_minutes()` 按它给本地日期与每个时刻的偏移。
//!
//! 内核的 crate 不建它（`check:rust-cli` 拦：只经注入的 `Clock`）。

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{civil, units};

/// 时区规则：入口读系统时区后实现，内核只经 `Clock` 用它。
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

    /// Unix 毫秒。
    #[expect(
        clippy::disallowed_methods,
        reason = "SystemClock 是业务时间的唯一来源（README §4 的时间类入口）"
    )]
    pub fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0)
    }

    /// 时区的名字。
    pub fn time_zone(&self) -> String {
        self.zone.name()
    }

    /// 修复（R2-6）：以前一律按 UTC；现在按入口给的时区、现在这一刻的偏移算本地日期 `YYYY-MM-DD`。
    pub fn today(&self) -> String {
        let now = self.now_ms();
        local_date(now, self.zone.offset_minutes(now))
    }

    /// `at_ms` 这一刻相对 UTC 的偏移（分钟）。
    pub fn utc_offset_minutes(&self, at_ms: u64) -> i32 {
        self.zone.offset_minutes(at_ms)
    }
}

/// `utc_ms` 加上偏移之后的日期 `YYYY-MM-DD`。
pub(crate) fn local_date(utc_ms: u64, offset_minutes: i32) -> String {
    let utc = i64::try_from(utc_ms).unwrap_or(i64::MAX);
    let local = utc.saturating_add(i64::from(offset_minutes) * units::MS_PER_MINUTE as i64);
    let local = u64::try_from(local).unwrap_or(0);
    civil::format_date((local / units::MS_PER_DAY) as i64)
}

#[cfg(test)]
#[path = "clock_tests.rs"]
mod tests;
