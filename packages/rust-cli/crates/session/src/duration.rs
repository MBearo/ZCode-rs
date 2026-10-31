//! 回合结束那一行的用时（docs/specs/rust-cli/tui-display.md §2.7，修复线 F118）。
//!
//! 会话 actor 是用时的唯一来源：回合开始到结束，扣掉回合里有待回答的交互的时间（询问框打开时停表）；回合结束时还有
//! 后台子代理没完成就写“等它们完成”，之后第一个不用等的回合从第一个等待的回合开始算。只在内存，不落库。

use rust_cli_kernel_api::ids::TurnId;
use rust_cli_kernel_api::metering::Metered;
use rust_cli_kernel_api::model::QuerySource;
use rust_cli_kernel_api::session_api::KernelEvent;
use rust_cli_kernel_api::turn_duration::TurnDuration;

use crate::actor::Actor;

/// 一个回合的计时（随 `Running`）。
#[derive(Debug, Clone, Copy)]
pub(crate) struct TurnTiming {
    started_ms: u64,
    paused_ms: u64,
    /// 正在等回答：从这一刻起停表。
    paused_since: Option<u64>,
    /// 这个回合发过主线程的模型请求（成功与失败都算）。没发过的回合（`/compact`、阻塞的输入、本地命令）没有这一行。
    queried: bool,
}

impl TurnTiming {
    /// 回合开始；那时已经有待回答的交互（例如后台子代理的询问）就从开始停表。
    pub(crate) fn start(now_ms: u64, asking: bool) -> Self {
        Self {
            started_ms: now_ms,
            paused_ms: 0,
            paused_since: asking.then_some(now_ms),
            queried: false,
        }
    }

    /// 待回答的交互从无到有时停表，从有到无时把停的时间累加。
    fn asking(&mut self, now_ms: u64, asking: bool) {
        match (self.paused_since, asking) {
            (None, true) => self.paused_since = Some(now_ms),
            (Some(since), false) => {
                self.paused_ms += now_ms.saturating_sub(since);
                self.paused_since = None;
            }
            _ => {}
        }
    }

    /// 开始到 `now_ms`，扣掉停表的时间（还在停时算到现在）。
    fn active_ms(&self, now_ms: u64) -> u64 {
        let pausing = self
            .paused_since
            .map_or(0, |since| now_ms.saturating_sub(since));
        now_ms
            .saturating_sub(self.started_ms)
            .saturating_sub(self.paused_ms + pausing)
    }

    /// 这一行的用时；`wait_started` 是会话里等后台子代理的起点。有要等的子代理时记下起点（已有的不动），用时是本回合的
    /// （前端不显示）；没有时起点有值就从起点算到现在（墙上时间，不扣停表），并清掉起点。
    fn duration_ms(&self, now_ms: u64, waiting: bool, wait_started: &mut Option<u64>) -> u64 {
        if waiting {
            wait_started.get_or_insert(self.started_ms);
            return self.active_ms(now_ms);
        }
        match wait_started.take() {
            Some(since) => now_ms.saturating_sub(since),
            None => self.active_ms(now_ms),
        }
    }
}

impl Actor {
    /// 待回答的交互变了（登记、回答、取消之后调用）：回合的计时按“有没有在等回答”停表或继续。
    pub(crate) fn asking_changed(&mut self) {
        let asking = !self.pending.is_empty();
        let now = self.deps.clock.now_ms();
        if let Some(running) = self.running.as_mut() {
            running.timing.asking(now, asking);
        }
    }

    /// 一次计量：主线程的模型请求说明这个回合“发过请求”（子代理与旁路的请求不在这里，压缩的来源不是主线程）。
    pub(crate) fn note_request(&mut self, metered: &Metered) {
        if let (Metered::Model(call), Some(running)) = (metered, self.running.as_mut())
            && call.source == QuerySource::MainThread
        {
            running.timing.queried = true;
        }
    }

    /// 回合结束那一行：`TurnFinished` 之后、开下一个回合之前发。中断与没发主线程请求的回合没有。
    pub(crate) async fn turn_duration(
        &mut self,
        turn: TurnId,
        timing: TurnTiming,
        interrupted: bool,
    ) {
        if interrupted || !timing.queried {
            return;
        }
        let waiting = self
            .deps
            .agents
            .background_agents(&self.meta.id)
            .await
            .len();
        let now = self.deps.clock.now_ms();
        let duration_ms = timing.duration_ms(now, waiting > 0, &mut self.wait_started);
        self.log.append(KernelEvent::TurnDuration(TurnDuration {
            turn,
            duration_ms,
            finished_at_ms: now,
            utc_offset_minutes: self.deps.clock.utc_offset_minutes(now),
            waiting_agents: u32::try_from(waiting).unwrap_or(u32::MAX),
        }));
    }
}

#[cfg(test)]
#[path = "duration_tests.rs"]
mod tests;
