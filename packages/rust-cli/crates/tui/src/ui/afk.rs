//! 提问框的空闲超时（docs/specs/rust-cli/tools-more.md §1.1，工作线 F72）：从提问框弹出或最后一次输入起计时，
//! 终端报告有焦点时不计；每秒检查一次，剩下的时间不超过倒计时门槛时显示倒计时，到期按当时已选的答案提交。
//! 计时、上一次输入与终端焦点都是界面状态，不同步任何业务状态。
use rust_cli_kernel_api::ids::InteractionId;
use rust_cli_kernel_api::orchestration::InteractionRequest;
use tokio::time::{Duration, Instant};

/// 倒计时门槛的缺省值。
const COUNTDOWN_MS: u64 = 20_000;
/// 检查的间隔。
const TICK: Duration = Duration::from_secs(1);

/// 入口给出的超时：配置的 `askUserQuestionTimeout` 或环境变量 `ZCODE_AFK_TIMEOUT_MS`（环境变量压过配置）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuestionTimeout {
    pub timeout_ms: u64,
    /// 剩下的时间不超过它时显示倒计时（`ZCODE_AFK_COUNTDOWN_MS`，缺省 20 s，不超过超时）。
    pub countdown_ms: u64,
}

impl QuestionTimeout {
    /// 配置值（`60s` / `5m` / `10m` 的毫秒，`never` 为 None）与两个环境变量；超时都没有时不计时。
    pub fn new(
        setting_ms: Option<u64>,
        env_ms: Option<u64>,
        countdown_ms: Option<u64>,
    ) -> Option<Self> {
        let timeout_ms = env_ms.or(setting_ms)?;
        Some(Self {
            timeout_ms,
            countdown_ms: countdown_ms.unwrap_or(COUNTDOWN_MS).min(timeout_ms),
        })
    }

    fn seconds(ms: u64) -> u64 {
        ms.div_ceil(1000)
    }

    /// 剩下的秒数要不要显示。
    fn shows(&self, seconds: u64) -> bool {
        seconds.saturating_mul(1000) <= self.countdown_ms
    }
}

/// 一次检查的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tick {
    /// 还没到期：要显示的倒计时（秒），不显示时为 None。
    Waiting(Option<u64>),
    /// 到期：按当时已选的答案提交，带超时的毫秒数。
    Expired(u64),
}

#[derive(Debug, Clone)]
struct Timer {
    id: InteractionId,
    /// 弹出的时刻（检查按它对齐到整秒）。
    opened: Instant,
    /// 计时的起点：弹出时刻，终端有焦点时每次检查都移到当时。
    start: Instant,
    fired: bool,
    /// 要显示的倒计时（秒）：弹出时与每次检查后更新。
    countdown: Option<u64>,
}

/// 空闲计时的界面状态。
#[derive(Debug, Clone, Default)]
pub struct Afk {
    config: Option<QuestionTimeout>,
    last_input: Option<Instant>,
    /// 终端报告的焦点；终端不报告时为 None（照常计时）。
    focused: Option<bool>,
    timer: Option<Timer>,
}

impl Afk {
    pub fn new(config: Option<QuestionTimeout>) -> Self {
        Self {
            config,
            ..Self::default()
        }
    }

    /// 终端输入（按键、粘贴、鼠标点击）：重新计时（“any key to stay”）。
    pub fn input(&mut self, now: Instant) {
        self.last_input = Some(now);
    }

    /// 终端报告焦点变化。
    pub fn focus(&mut self, focused: bool) {
        self.focused = Some(focused);
    }

    /// 视图里的待处理交互（每次视图变化后）：换了提问就重新计时；不是提问时停。
    pub fn dialog(&mut self, pending: Option<(&InteractionId, &InteractionRequest)>, now: Instant) {
        let Some(config) = self.config else {
            return;
        };
        let Some((id, InteractionRequest::Question { .. })) = pending else {
            self.timer = None;
            return;
        };
        if self.timer.as_ref().is_none_or(|timer| timer.id != *id) {
            let seconds = QuestionTimeout::seconds(config.timeout_ms);
            self.timer = Some(Timer {
                id: id.clone(),
                opened: now,
                start: now,
                fired: false,
                countdown: config.shows(seconds).then_some(seconds),
            });
        }
    }

    /// 提问框下面要显示的倒计时（秒）。
    pub fn countdown(&self) -> Option<u64> {
        self.timer.as_ref().and_then(|timer| timer.countdown)
    }

    /// 下一次检查的时刻：弹出之后的整秒；没有在计时的提问时为 None。
    pub fn next_due(&self, now: Instant) -> Option<Instant> {
        let timer = self.timer.as_ref().filter(|timer| !timer.fired)?;
        let ticks = now.saturating_duration_since(timer.opened).as_millis() / TICK.as_millis();
        let ticks = u32::try_from(ticks + 1).unwrap_or(u32::MAX);
        Some(timer.opened + TICK * ticks)
    }

    /// 到点的检查；没有在计时的提问时为 None。
    pub fn tick(&mut self, now: Instant) -> Option<Tick> {
        let config = self.config?;
        let timer = self.timer.as_mut().filter(|timer| !timer.fired)?;
        if self.focused == Some(true) {
            timer.start = now;
        }
        let since = self
            .last_input
            .map_or(timer.start, |input| input.max(timer.start));
        let elapsed =
            u64::try_from(now.saturating_duration_since(since).as_millis()).unwrap_or(u64::MAX);
        if elapsed >= config.timeout_ms {
            timer.fired = true;
            timer.countdown = None;
            return Some(Tick::Expired(config.timeout_ms));
        }
        let seconds = if elapsed >= config.timeout_ms - config.countdown_ms {
            QuestionTimeout::seconds(config.timeout_ms - elapsed)
        } else {
            QuestionTimeout::seconds(config.timeout_ms)
        };
        timer.countdown = config.shows(seconds).then_some(seconds);
        Some(Tick::Waiting(timer.countdown))
    }
}

#[cfg(test)]
#[path = "afk_tests.rs"]
mod tests;
