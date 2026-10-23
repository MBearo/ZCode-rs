//! 流空闲看门狗（docs/specs/rust-cli/stall-visibility.md §2）：事件级与字节级。
//!
//! 配置由入口给出（环境变量只在 cli 读，README §4）；这里按端点算出上限，读流时记下最近一块字节与最近一个
//! SSE 事件的时刻，给出下一个要醒来的时刻：停顿通知（20 s 没有字节）或超过上限。时刻用 tokio 的单调时钟，
//! 只决定这次流什么时候中止，不用来同步任何状态。

use std::time::Duration;

use tokio::time::Instant;

/// 事件级上限的下限：环境变量只能把它调大（`max(env || 0, 300000)`）。
pub const EVENT_IDLE_FLOOR: Duration = Duration::from_secs(300);
/// 字节级上限夹在这个范围里（`[10000, 1800000]`）。
pub const BYTE_IDLE_MIN: Duration = Duration::from_secs(10);
pub const BYTE_IDLE_MAX: Duration = Duration::from_secs(1800);
/// 多久没有字节就发停顿通知。
pub const STALL_NOTICE: Duration = Duration::from_secs(20);
/// 连续的 `ping` 只有前这么多个重新计时。
const COUNTED_PINGS: u32 = 30;

/// 字节级上限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteIdle {
    /// 按端点：请求方案的缺省（官方 Anthropic 端点 180 s，其他 300 s；model.md §3.2）。
    Endpoint,
    Fixed(Duration),
}

/// 看门狗的配置。`None` 表示关掉那一个看门狗。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamIdle {
    /// 事件级上限：每个 SSE 事件重新计时。
    pub events: Option<Duration>,
    /// 字节级上限：每块字节重新计时。
    pub bytes: Option<ByteIdle>,
    /// 多久没有字节就发停顿通知。
    pub stall_notice: Duration,
}

impl Default for StreamIdle {
    /// 缺省：两个看门狗都开，事件级 300 s，字节级按端点。
    fn default() -> Self {
        Self {
            events: Some(EVENT_IDLE_FLOOR),
            bytes: Some(ByteIdle::Endpoint),
            stall_notice: STALL_NOTICE,
        }
    }
}

impl StreamIdle {
    /// 这个端点上的上限；`endpoint` 是请求方案给的字节级缺省上限（修复线 F23：以前按 `is_first_party`）。
    pub(crate) fn limits(&self, endpoint: Duration) -> Limits {
        let bytes = self.bytes.map(|bytes| match bytes {
            ByteIdle::Endpoint => endpoint,
            ByteIdle::Fixed(limit) => limit,
        });
        Limits {
            events: self.events,
            bytes,
            stall: self.stall_notice,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Limits {
    events: Option<Duration>,
    bytes: Option<Duration>,
    stall: Duration,
}

/// 读流时为什么醒来。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Wake {
    /// 发停顿通知。
    Stall,
    /// 超过了这个上限：中止这次流。
    Idle(Duration),
}

/// 一次流的计时。
#[derive(Debug)]
pub(crate) struct Watch {
    limits: Limits,
    last_byte: Instant,
    last_event: Instant,
    pings: u32,
    /// 这段安静期已经发过停顿通知（收到下一个计数的事件时结束）。
    stalled: bool,
}

impl Watch {
    /// 从拿到响应头的时刻开始计时。
    pub(crate) fn new(limits: Limits, now: Instant) -> Self {
        Self {
            limits,
            last_byte: now,
            last_event: now,
            pings: 0,
            stalled: false,
        }
    }

    /// 最近的中止时刻与那个上限；两个看门狗都关时为 None。
    fn deadline(&self) -> Option<(Instant, Duration)> {
        let bytes = self
            .limits
            .bytes
            .map(|limit| (self.last_byte + limit, limit));
        let events = self
            .limits
            .events
            .map(|limit| (self.last_event + limit, limit));
        match (bytes, events) {
            (Some(a), Some(b)) => Some(if b.0 < a.0 { b } else { a }),
            (one, other) => one.or(other),
        }
    }

    /// 下一个要醒来的时刻与原因。停顿通知只在有中止时刻时发（它要告诉用户还有多久）。
    pub(crate) fn next(&self) -> Option<(Instant, Wake)> {
        let (deadline, limit) = self.deadline()?;
        let stall = self.last_byte + self.limits.stall;
        if !self.stalled && stall < deadline {
            return Some((stall, Wake::Stall));
        }
        Some((deadline, Wake::Idle(limit)))
    }

    /// 停顿通知到点：记下已经发过，返回离中止还有多久。
    pub(crate) fn stall(&mut self, now: Instant) -> Duration {
        self.stalled = true;
        self.deadline().map_or(Duration::ZERO, |(deadline, _)| {
            deadline.saturating_duration_since(now)
        })
    }

    /// 收到一块字节。
    pub(crate) fn bytes(&mut self, now: Instant) {
        self.last_byte = now;
    }

    /// 收到一个 SSE 事件。返回之前是否发过停顿通知（调用方先交付 `Resumed`）。连续 `ping` 超过
    /// [`COUNTED_PINGS`] 个之后的 `ping` 不算（不重新计时，也不清掉停顿）。
    pub(crate) fn event(&mut self, ping: bool, now: Instant) -> bool {
        if ping {
            self.pings += 1;
            if self.pings > COUNTED_PINGS {
                return false;
            }
        } else {
            self.pings = 0;
        }
        self.last_event = now;
        std::mem::replace(&mut self.stalled, false)
    }
}

#[cfg(test)]
#[path = "idle_tests.rs"]
mod tests;
