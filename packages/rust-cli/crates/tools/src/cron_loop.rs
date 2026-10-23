//! ScheduleWakeup 的 loop 唤醒：排程、停止、每个 prompt 的链与 keepalive（docs/specs/rust-cli/tools-more.md
//! §17.2、§17.3）。
//!
//! loop 唤醒是一个一次性会话任务（`<分> <时> * * *`，种类 loop），由 §10.4 的同一条路径在空闲时触发。

use std::collections::HashMap;

use rust_cli_kernel_api::ids::SessionId;

use crate::cron_board::{CronBoard, Job, JobKind, State};
use crate::cron_expr::{self, RECURRING_MAX_AGE_MS};

/// 时长的下限与上限（秒）。
const MIN_DELAY_S: i64 = 60;
const MAX_DELAY_S: i64 = 3_600;
/// keepalive 排的时长与预算。
const KEEPALIVE_DELAY_S: f64 = 1_200.0;
const KEEPALIVE_BUDGET: u32 = 1;
/// cache 的提前量与 5 分钟的 prompt cache 窗口：值取 cron_expr（F55：以前各写一份，这里用 u64）。
const CACHE_LEAD_MS: u64 = cron_expr::CACHE_LEAD_MS as u64;
const CACHE_WINDOW_MS: u64 = cron_expr::CACHE_WINDOW_MS as u64;
const MINUTE_MS: u64 = cron_expr::MINUTE_MS as u64;

/// 一个 prompt 的链。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Chain {
    started_at: u64,
    last_scheduled_for: u64,
    aged_out: bool,
}

/// 一个顶层会话的 loop 状态（链、在跑的 tick 与连续 keepalive 次数）。
#[derive(Debug, Default)]
pub(crate) struct LoopState {
    chains: HashMap<String, Chain>,
    /// 触发后还没结束的 loop tick 的 prompt。
    pub(crate) in_flight: Option<String>,
    keepalives: u32,
}

/// 排程的结果（ScheduleWakeup 的 `data`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scheduled {
    At {
        scheduled_for: u64,
        clamped: i64,
        was_clamped: bool,
    },
    /// 链满 7 天，不再排（结果是 `scheduledFor: 0`）。
    Ended,
}

/// 夹到 [60, 3600] 秒，目标向上取整到整分钟，短时长按分钟往前挪到 cache 窗口里。
/// 返回（夹过的秒数、是否夹过、目标时间、任务的 createdAt）。
pub(crate) fn clamp(delay: f64, now: u64) -> (i64, bool, u64, u64) {
    let rounded = if delay.is_nan() || delay == f64::NEG_INFINITY {
        MIN_DELAY_S
    } else if delay == f64::INFINITY {
        MAX_DELAY_S
    } else {
        // JS `Math.round`：.5 向正无穷。
        (delay + 0.5).floor() as i64
    };
    let clamped = rounded.clamp(MIN_DELAY_S, MAX_DELAY_S);
    let was_clamped = !delay.is_finite() || rounded != clamped;
    let at = now + clamped as u64 * 1_000;
    // 本地时间的分钟边界；偏移是整分钟，所以与 UTC 的分钟边界相同。
    let mut target = at.div_ceil(MINUTE_MS) * MINUTE_MS;
    if clamped as u64 * 1_000 <= CACHE_WINDOW_MS {
        let limit = CACHE_WINDOW_MS - CACHE_LEAD_MS;
        while target - now > limit && target - MINUTE_MS >= now + MIN_DELAY_S as u64 * 1_000 {
            target -= MINUTE_MS;
        }
    }
    let created_at = if at < target { at } else { target - 1 };
    (clamped, was_clamped, target, created_at)
}

impl CronBoard {
    /// ScheduleWakeup：删掉本会话已有的 loop 唤醒，排一个新的。
    pub(crate) fn schedule_wakeup(
        &self,
        owner: &SessionId,
        id: String,
        delay: f64,
        prompt: &str,
    ) -> Scheduled {
        let scheduled = schedule(self, &mut self.state(), owner, id, delay, prompt, false);
        self.changed();
        scheduled
    }

    /// `stop: true`：删掉本会话所有 loop 唤醒，清掉在跑的 tick 与 keepalive 计数，忘掉这些链。
    pub(crate) fn stop_loop(&self, owner: &SessionId) -> usize {
        let mut state = self.state();
        let removed = remove_wakeups(&mut state, owner);
        let lp = state.loops.entry(owner.clone()).or_default();
        lp.keepalives = 0;
        let in_flight = lp.in_flight.take();
        for prompt in removed.iter().chain(in_flight.iter()) {
            lp.chains.remove(prompt);
        }
        drop(state);
        self.changed();
        removed.len()
    }

    /// keepalive（在会话空闲取到期任务时判断）：上一个 loop tick 结束了、模型没有排新的唤醒也
    /// 没有停止 → 预算没用完时用同一个 prompt 排 1200 秒，否则循环结束。
    pub(crate) fn keepalive(&self, owner: &SessionId) {
        let mut state = self.state();
        let Some(lp) = state.loops.get_mut(owner) else {
            return;
        };
        let Some(prompt) = lp.in_flight.take() else {
            return;
        };
        let pending = state
            .jobs
            .get(owner)
            .is_some_and(|jobs| jobs.iter().any(|job| job.kind == JobKind::Loop));
        let exhausted = state
            .loops
            .get(owner)
            .is_some_and(|lp| lp.keepalives >= KEEPALIVE_BUDGET);
        if pending || exhausted {
            return;
        }
        let id = format!("{:08x}", self.now_ms() & 0xffff_ffff);
        schedule(
            self,
            &mut state,
            owner,
            id,
            KEEPALIVE_DELAY_S,
            &prompt,
            true,
        );
        drop(state);
        self.changed();
    }
}

fn remove_wakeups(state: &mut State, owner: &SessionId) -> Vec<String> {
    let Some(jobs) = state.jobs.get_mut(owner) else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    jobs.retain(|job| {
        let wakeup = job.kind == JobKind::Loop;
        if wakeup {
            removed.push(job.prompt.clone());
        }
        !wakeup
    });
    removed
}

/// 链满 7 天时结束；否则加一个一次性 loop 任务并记下链。
fn schedule(
    board: &CronBoard,
    state: &mut State,
    owner: &SessionId,
    id: String,
    delay: f64,
    prompt: &str,
    keepalive: bool,
) -> Scheduled {
    if !keepalive {
        state.loops.entry(owner.clone()).or_default().keepalives = 0;
    }
    remove_wakeups(state, owner);
    let now = board.now_ms();
    let lp = state.loops.entry(owner.clone()).or_default();
    let chain = lp.chains.get(prompt).copied();
    let broken =
        chain.is_some_and(|chain| now > chain.last_scheduled_for + MAX_DELAY_S as u64 * 1_000);
    let started_at = match chain {
        Some(chain) if !broken => chain.started_at,
        _ => now,
    };
    if now.saturating_sub(started_at) >= RECURRING_MAX_AGE_MS {
        if !chain.is_some_and(|chain| chain.aged_out) {
            // 一分钟后链算断开，可以重新开始（起点记为 now 减去上限与下限之差）。
            let last = now.saturating_sub((MAX_DELAY_S - MIN_DELAY_S) as u64 * 1_000);
            lp.chains.insert(
                prompt.to_owned(),
                Chain {
                    started_at,
                    last_scheduled_for: last,
                    aged_out: true,
                },
            );
        }
        return Scheduled::Ended;
    }
    let (clamped, was_clamped, target, created_at) = clamp(delay, now);
    let offset = board.offset();
    let local = target as i64 / MINUTE_MS as i64 + i64::from(offset(target));
    let cron = format!(
        "{} {} * * *",
        local.rem_euclid(60),
        local.div_euclid(60).rem_euclid(24)
    );
    let next_fire = cron_expr::one_shot_next(&cron, created_at, &id, &offset);
    lp.chains.insert(
        prompt.to_owned(),
        Chain {
            started_at,
            last_scheduled_for: target,
            aged_out: false,
        },
    );
    if keepalive {
        lp.keepalives += 1;
    }
    state.jobs.entry(owner.clone()).or_default().push(Job {
        id,
        cron,
        prompt: prompt.to_owned(),
        recurring: false,
        created_at,
        kind: JobKind::Loop,
        next_fire,
    });
    state.enabled = true;
    Scheduled::At {
        scheduled_for: target,
        clamped,
        was_clamped,
    }
}

#[cfg(test)]
#[path = "cron_loop_tests.rs"]
mod tests;
