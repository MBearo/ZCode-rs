//! 测试用的时钟（F126）：固定、手动拨、跟着 tokio 时间走、每读一次前进一步。以前 12 个 crate 各写一份，只有这几种写法。

use std::sync::atomic::{AtomicU64, Ordering};

use crate::clock::Clock;

const UTC: &str = "UTC";

/// 固定在给定的 Unix 毫秒，时区 UTC。
pub struct FixedClock(pub u64);

impl Clock for FixedClock {
    fn now_ms(&self) -> u64 {
        self.0
    }

    fn time_zone(&self) -> String {
        UTC.to_owned()
    }
}

/// 测试拨动的时钟（缺省在 0、时区 UTC）。
#[derive(Default)]
pub struct ManualClock {
    now: AtomicU64,
    /// 时区名与它相对 UTC 的偏移（分钟）；None 是 UTC。
    zone: Option<(String, i32)>,
}

impl ManualClock {
    pub fn new(now_ms: u64) -> Self {
        Self {
            now: AtomicU64::new(now_ms),
            zone: None,
        }
    }

    /// 换成时区 `name`，相对 UTC 的偏移固定是 `offset_minutes`。
    #[must_use]
    pub fn with_zone(mut self, name: &str, offset_minutes: i32) -> Self {
        self.zone = Some((name.to_owned(), offset_minutes));
        self
    }

    pub fn set(&self, now_ms: u64) {
        self.now.store(now_ms, Ordering::SeqCst);
    }

    pub fn advance(&self, by_ms: u64) {
        self.now.fetch_add(by_ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }

    fn time_zone(&self) -> String {
        self.zone
            .as_ref()
            .map_or_else(|| UTC.to_owned(), |(name, _)| name.clone())
    }

    fn utc_offset_minutes(&self, _at_ms: u64) -> i32 {
        self.zone.as_ref().map_or(0, |(_, offset)| *offset)
    }
}

/// 跟着 tokio 时间走的时钟：建出时是 `base_ms`，之后加上经过的时间（暂停的测试时间里也准），时区 UTC。
pub struct TokioClock {
    base_ms: u64,
    start: tokio::time::Instant,
}

impl TokioClock {
    pub fn new(base_ms: u64) -> Self {
        Self {
            base_ms,
            start: tokio::time::Instant::now(),
        }
    }
}

impl Clock for TokioClock {
    fn now_ms(&self) -> u64 {
        let elapsed = u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.base_ms.saturating_add(elapsed)
    }

    fn time_zone(&self) -> String {
        UTC.to_owned()
    }
}

/// 每读一次前进 `step` 的时钟：依次读到 `start`、`start + step`……（计时点的先后可以逐个核对），时区 UTC。
pub struct StepClock {
    next: AtomicU64,
    step: u64,
}

impl StepClock {
    pub fn new(start: u64, step: u64) -> Self {
        Self {
            next: AtomicU64::new(start),
            step,
        }
    }
}

impl Clock for StepClock {
    fn now_ms(&self) -> u64 {
        self.next.fetch_add(self.step, Ordering::SeqCst)
    }

    fn time_zone(&self) -> String {
        UTC.to_owned()
    }
}
