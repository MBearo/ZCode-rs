//! 一个 monitor 的切行、批与限流（工作线 F83 新增，docs/specs/rust-cli/tools-more.md §19.6）：纯状态，没有 IO、计时器
//! 与文字。
//!
//! 主线程的 monitor 由会话 actor 持有它，子代理的由 agents 的转发任务持有（同一份规则，tools-more.md §19.1）：输出到了调
//! [`Watch::on_data`]（ws 的生命周期调 [`Watch::on_lifecycle`]），批的截止时刻到了调 [`Watch::flush_due`]，到期调
//! [`Watch::expire`]，事件流结束调 [`Watch::finish`]。说明是结构化的 [`Housekeeping`]，文字由调用方按 prompts 给出。
//! 时刻是注入的时钟给的毫秒数（README §4：内核不用单调时钟的类型），由调用方传入，所以测试不用等。

use rust_cli_base::js::string::{trim, utf16_len, utf16_prefix};
use rust_cli_base::units;

use crate::env_shell::SocketEvent;
use crate::monitor::MonitorArmed;

/// 输出流连续出错这么多次就不再按游标重新订阅（这个 monitor 之后只等终态通知）。命令结束后 `watch_command` 给
/// `NotFound`（工作线 F119），所以会话 actor 与 agents 的转发任务都要有这个上限，不然一直重新订阅。
pub const STREAM_RETRIES: u8 = 3;

/// 一行最多这么多 UTF-16 单位。
const LINE_LIMIT: usize = 500;
/// 一批最多这么多 UTF-16 单位。
const BATCH_LIMIT: usize = 3_000;
/// 第一行起多久成一批（毫秒）。
pub const BATCH_DELAY: u64 = 200;
/// 行缓冲的上限（字节）。
const BUFFER_LIMIT: usize = 1_048_576;
/// 令牌桶：容量与补一个的间隔（毫秒）。
const BUCKET: u32 = 10;
const REFILL: u64 = 2_000;
/// 连续压下超过这么久就停掉（毫秒）。
const SUPPRESS_LIMIT: u64 = 30_000;
/// 最后一次压下之后安静这么多个补充周期，压下的计时重新开始。
const QUIET_REFILLS: u64 = 3;

/// 交给通道的一条通知（`<event>` 里的内容，未转义）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Emit {
    /// 一批事件（受限流）。
    Event(String),
    /// 说明，不经限流。
    Notice(Housekeeping),
}

/// 说明的种类（文字见 `rust_cli_prompts::monitor`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Housekeeping {
    /// 压下之后第一次拿到令牌：压下的批数。
    Suppressed(u64),
    /// 压下太久、停掉：压下的批数与秒数。
    TooMuchOutput { suppressed: u64, seconds: u64 },
    /// 到期：期限（毫秒）与已送出的事件数。
    Expired { timeout_ms: u64, events: u64 },
    /// ws 出错、升级被拒、帧太大、关闭。
    Socket(SocketEvent),
    /// 用户停掉了 ws 来源。
    Stopped,
}

/// 一次处理的结果：要送的通知，以及要不要停掉命令（到期、输出太多）。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Step {
    pub emits: Vec<Emit>,
    pub stop: bool,
}

/// 令牌桶：开始时满，每 `REFILL` 补一个，不超过容量。
#[derive(Debug, Clone)]
struct Bucket {
    tokens: u32,
    since: u64,
}

impl Bucket {
    fn refill(&mut self, now: u64) {
        let gained = now.saturating_sub(self.since) / REFILL;
        if gained > 0 {
            self.tokens = u32::try_from(gained)
                .unwrap_or(u32::MAX)
                .saturating_add(self.tokens)
                .min(BUCKET);
            self.since += REFILL * gained;
        }
    }

    fn take(&mut self, now: u64) -> bool {
        self.refill(now);
        if self.tokens > 0 {
            self.tokens -= 1;
            return true;
        }
        false
    }
}

pub struct Watch {
    pub armed: MonitorArmed,
    /// 已读到的输出位置（`watch_command` 的游标）。
    pub cursor: u64,
    /// 还没有换行的半行（字节）。
    buffer: Vec<u8>,
    pending: Vec<String>,
    batch_due: Option<u64>,
    bucket: Bucket,
    /// 上次说明之后压下的批数、最后一次压下的时刻、这一段压下开始的时刻。
    suppressed: u64,
    last_suppressed: Option<u64>,
    suppress_start: Option<u64>,
    /// 已送出的事件数（到期说明用）。
    pub events: u64,
    pub expires: u64,
    /// 已经到期（发过说明、停过命令）。
    pub expired: bool,
    /// 关闭之后不再送事件（输出太多、流已结束）。
    pub closed: bool,
    /// 停掉过（输出太多），或 ws 已关闭：之后的 ws 生命周期不再送，用户停掉的说明也不送。
    pub stopped: bool,
    /// ws：升级被拒之后不再送出错说明；一帧太大之后不再送关闭说明（关闭时静默）。
    rejected: bool,
    dropped: bool,
    /// ws 生命周期还没有换行的半行。
    lifecycle: Vec<u8>,
}

/// 一行的写法：去掉首尾空白，太长的截断。空行为 None。
fn line(raw: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(raw);
    let text = trim(&text);
    if text.is_empty() {
        return None;
    }
    if utf16_len(text) > LINE_LIMIT {
        return Some(format!("{}...(truncated)", utf16_prefix(text, LINE_LIMIT)));
    }
    Some(text.to_owned())
}

impl Watch {
    pub fn new(armed: MonitorArmed, now: u64) -> Self {
        let expires = now.saturating_add(armed.timeout_ms);
        Self {
            armed,
            cursor: 0,
            buffer: Vec::new(),
            pending: Vec::new(),
            batch_due: None,
            bucket: Bucket {
                tokens: BUCKET,
                since: now,
            },
            suppressed: 0,
            last_suppressed: None,
            suppress_start: None,
            events: 0,
            expires,
            expired: false,
            closed: false,
            stopped: false,
            rejected: false,
            dropped: false,
            lifecycle: Vec::new(),
        }
    }

    /// 最近的截止时刻：批，或还没到期时的到期时刻。
    pub fn deadline(&self) -> Option<u64> {
        let expiry = (!self.expired && !self.closed).then_some(self.expires);
        match (self.batch_due, expiry) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// 新的 stdout 数据。
    pub fn on_data(&mut self, data: &[u8], now: u64) {
        self.buffer.extend_from_slice(data);
        if self.buffer.len() > BUFFER_LIMIT {
            let drop = self.buffer.len() - BUFFER_LIMIT;
            self.buffer.drain(..drop);
        }
        while let Some(at) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let raw: Vec<u8> = self.buffer.drain(..=at).collect();
            if let Some(line) = line(&raw[..raw.len() - 1]) {
                self.pending.push(line);
            }
        }
        if !self.pending.is_empty() && self.batch_due.is_none() {
            self.batch_due = Some(now.saturating_add(BATCH_DELAY));
        }
    }

    /// 取出待发的行：`last` 时连同没有换行的半行。
    fn take_batch(&mut self, last: bool) -> Option<String> {
        self.batch_due = None;
        if last {
            let rest = std::mem::take(&mut self.buffer);
            if let Some(line) = line(&rest) {
                self.pending.push(line);
            }
        }
        if self.pending.is_empty() {
            return None;
        }
        let joined = std::mem::take(&mut self.pending).join("\n");
        if utf16_len(&joined) > BATCH_LIMIT {
            return Some(format!(
                "{}\n...(truncated)",
                utf16_prefix(&joined, BATCH_LIMIT)
            ));
        }
        Some(joined)
    }

    /// 拿一个令牌；拿到时先补一条压下说明。
    fn admit(&mut self, now: u64, step: &mut Step) -> bool {
        if !self.bucket.take(now) {
            self.suppressed += 1;
            self.last_suppressed = Some(now);
            self.suppress_start.get_or_insert(now);
            return false;
        }
        if self.suppressed > 0 {
            step.emits
                .push(Emit::Notice(Housekeeping::Suppressed(self.suppressed)));
            self.suppressed = 0;
            if self
                .last_suppressed
                .is_some_and(|last| now.saturating_sub(last) > REFILL * QUIET_REFILLS)
            {
                self.suppress_start = None;
            }
        }
        self.events += 1;
        true
    }

    /// 一批交给限流。
    fn deliver(&mut self, batch: String, now: u64, step: &mut Step) {
        if self.closed {
            return;
        }
        if self.admit(now, step) {
            step.emits.push(Emit::Event(batch));
            return;
        }
        if let Some(start) = self.suppress_start
            && now.saturating_sub(start) > SUPPRESS_LIMIT
        {
            self.closed = true;
            self.stopped = true;
            step.stop = true;
            // Math.round(毫秒 / 1000)。
            let seconds = units::round_ms_to_seconds(now.saturating_sub(start));
            step.emits.push(Emit::Notice(Housekeeping::TooMuchOutput {
                suppressed: self.suppressed,
                seconds,
            }));
        }
    }

    /// 批的截止时刻到了：送出待发的行。
    pub fn flush_due(&mut self, now: u64) -> Step {
        let mut step = Step::default();
        if self.batch_due.is_some_and(|due| due <= now)
            && let Some(batch) = self.take_batch(false)
        {
            self.deliver(batch, now, &mut step);
        }
        step
    }

    /// 到期：先送出待发的行（不含半行），再送到期说明，然后停掉命令。
    pub fn expire(&mut self, now: u64) -> Step {
        let mut step = Step::default();
        if self.closed || self.expired {
            return step;
        }
        if let Some(batch) = self.take_batch(false) {
            self.deliver(batch, now, &mut step);
        }
        if self.closed {
            return step;
        }
        self.expired = true;
        step.stop = true;
        step.emits.push(Emit::Notice(Housekeeping::Expired {
            timeout_ms: self.armed.timeout_ms,
            events: self.events,
        }));
        step
    }

    /// 截止时刻到了时的全部处理：到点的批，再看是否到期。
    pub fn on_due(&mut self, now: u64) -> Step {
        let mut step = self.flush_due(now);
        if !self.expired && !self.closed && self.expires <= now {
            let expiry = self.expire(now);
            step.emits.extend(expiry.emits);
            step.stop |= expiry.stop;
        }
        step
    }

    /// 送出待发的行，连同半行（ws 关闭时），然后关闭。
    pub fn flush_and_close(&mut self, now: u64) -> Step {
        let mut step = Step::default();
        if !self.closed
            && let Some(batch) = self.take_batch(true)
        {
            self.deliver(batch, now, &mut step);
        }
        self.closed = true;
        step
    }

    /// 事件流结束：取出剩下的行（含半行）并关闭；已关闭时为 None。
    pub fn finish(&mut self) -> Option<String> {
        if self.closed {
            return None;
        }
        self.closed = true;
        self.take_batch(true)
    }

    /// 剩下的行能不能送（拿一个令牌，可能先补压下说明）。
    pub fn admit_rest(&mut self, now: u64, step: &mut Step) -> bool {
        self.admit(now, step)
    }

    /// 被停掉、事件流结束之后的收尾：剩下的行拿得到令牌就作为普通事件；用户停掉的 ws 来源另送 `Stopped`（到期与
    /// 输出太多是调用方停的，说明已经送过）。
    pub fn after_stop(&mut self, rest: Option<String>, socket: bool, now: u64) -> Step {
        let mut step = Step::default();
        if let Some(rest) = rest
            && self.admit(now, &mut step)
        {
            step.emits.push(Emit::Event(rest));
        }
        if socket && !self.expired && !self.stopped {
            step.emits.push(Emit::Notice(Housekeeping::Stopped));
        }
        step
    }

    /// ws 的生命周期（执行环境写的 JSON 行，[`SocketEvent`]）：按到达顺序给说明。关闭前先送出待发的行（含半行）；
    /// 升级被拒之后不送出错说明；一帧太大之后不送关闭说明；到期或停掉过之后什么都不送。
    pub fn on_lifecycle(&mut self, data: &[u8], now: u64) -> Step {
        self.lifecycle.extend_from_slice(data);
        let mut step = Step::default();
        if self.expired || self.stopped {
            self.lifecycle.clear();
            return step;
        }
        while let Some(at) = self.lifecycle.iter().position(|byte| *byte == b'\n') {
            let raw: Vec<u8> = self.lifecycle.drain(..=at).collect();
            let Ok(event) = serde_json::from_slice::<SocketEvent>(&raw[..raw.len() - 1]) else {
                continue;
            };
            match &event {
                SocketEvent::Error { .. } if self.rejected => continue,
                SocketEvent::Error { .. } => {}
                SocketEvent::Rejected { .. } => self.rejected = true,
                SocketEvent::Dropped { .. } => self.dropped = true,
                SocketEvent::Closed { .. } => {
                    self.stopped = true;
                    let flushed = self.flush_and_close(now);
                    step.emits.extend(flushed.emits);
                    if self.dropped {
                        continue;
                    }
                }
            }
            step.emits.push(Emit::Notice(Housekeeping::Socket(event)));
        }
        step
    }
}

#[cfg(test)]
#[path = "monitor_watch_tests.rs"]
mod tests;
