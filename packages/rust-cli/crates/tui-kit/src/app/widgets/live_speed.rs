//! 状态栏的现场速度（docs/puffin-tui.md §4.2「多少」的「现场的速度」）：模型调用流式时，按墙钟给到达的字计时。
//! 从 Puffin `shell/live_speed.rs` 搬进 kit（tui-kit.md §4.2「footer」），旧界面与 Puffin 调同一份：壳把自己的数据映射成中性的
//! [`Heard`]（Puffin 的投影 `usage.live` 与 `usage.speed.last`，旧界面的视图）再调 [`LiveSpeed::heard`]。
//! 移植自 packages/tui/tui/src/zcode/tui/shell/LiveSpeed.scala（feat/tui a2bf878b34）第 1–158 行。
//!
//! 显示的是上一次调用量到的数；调用流式时是这次调用到此为止的平均，按它写的字估算。纯函数：壳给墙钟（毫秒）与 tick。
use crate::app::widgets::fade;
use crate::app::widgets::fit::rate;
use crate::app::widgets::playhead;
use crate::render::{Color, Span, Style};
use crate::theme::rgb::Rgb;
use crate::theme::theme::Theme;

/// 很久以前的 tick（同 Puffin 状态的 `LONG_AGO`）：从这时起的淡入早已落定。
const LONG_AGO: i64 = -1_000_000;

/// 一次听说的会话用量（Puffin 投影的 `usage.live` 与 `usage.speed.last`）。
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Heard {
    /// 最新回合里模型写下的字符（码点）：回答、思考与工具的输入。
    pub chars: u64,
    /// 其中的宽字符（中日韩、全角）。
    pub wide: u64,
    /// 回合在模型手里：在跑，没有待处理的请求，没有在运行的工具（送进后台的不算）。
    pub model_turn: bool,
    /// 最近一次结束的模型调用：键（每次调用不同）与它量到的输出 token；还没有时为 None。
    pub last_call: Option<(String, u64)>,
    /// 量到的速度：最近一次输出至少 50 个 token 的调用。
    pub speed_last: Option<f64>,
}

/// 速度停住多久之后变暗（毫秒）：这么久没有新字，数停在原处、变暗。
pub const HOLD_MS: f64 = 1000.0;
/// 调用的第一个字之后多久显示现场的数（毫秒）。
pub const SHOW_AFTER_MS: f64 = 500.0;
/// 两次重看现场的数之间的 tick 数（约 0.25 s）。
pub const REFRESH_TICKS: i64 = 8;
/// 现场的数变动至少这么多的比例（且至少 1）才换。
pub const STEP: f64 = 0.03;
/// 每个 token 的字数：窄字（拉丁等）与宽字（中日韩）。
pub const NARROW_PER_TOKEN: f64 = 3.9;
pub const WIDE_PER_TOKEN: f64 = 1.4;

/// 速度的墨色：亮（模型在写，前景）、暗（等或停住，fg3）、闲（回合不在模型手里，fg2）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Ink {
    Lit,
    Dim,
    #[default]
    Idle,
}

/// 一次模型调用：第一个字与最新一个字的墙钟毫秒。
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Call {
    pub start_ms: f64,
    pub last_ms: f64,
}

/// 写出的字：字数与其中宽字的个数。
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Written {
    pub chars: f64,
    pub wide: f64,
}

impl Written {
    /// `self - o`，每一项不小于 0。
    pub fn minus(self, o: Written) -> Written {
        Written {
            chars: (self.chars - o.chars).max(0.0),
            wide: (self.wide - o.wide).max(0.0),
        }
    }
}

/// 一段模型写下的字的字数与其中的宽字（码点；宽字是 `unicode-width` 宽 2 的，中日韩、全角）：同 Puffin fold 的
/// `count_live`，旧界面的视图按它数（[`Heard::chars`]、[`Heard::wide`]）。
pub fn written(text: &str) -> (u64, u64) {
    use unicode_width::UnicodeWidthChar;
    let chars = text.chars().count() as u64;
    let wide = text.chars().filter(|c| c.width() == Some(2)).count() as u64;
    (chars, wide)
}

/// `w` 估算的 token 数（校准之前）。
pub fn tokens(w: Written) -> f64 {
    (w.chars - w.wide) / NARROW_PER_TOKEN + w.wide / WIDE_PER_TOKEN
}

fn ink_style(theme: &Theme, ink: Ink) -> Style {
    match ink {
        Ink::Lit => theme
            .fg
            .map_or(Style::DEFAULT, |c| Style::fg(Rgb::color(c))),
        Ink::Dim => theme.fg3,
        Ink::Idle => theme.fg2,
    }
}

/// 状态栏的速度。
///
/// - `shown`：状态栏说的；`None` 不显示。`was` 是上次变之前说的，`changed_at` 是那次变的 tick。
/// - `ink`：墨色要到的；`ink_from` 是之前的，从 tick `ink_at` 起。
/// - `call`：正在写的调用。`seen` 是最新回合上次告诉的字数；`base` 是这次调用开始时的。
/// - `model_turn`：回合在模型手里。`last_key`：最新一次结束的调用的键。`primed`：告诉过一次（第一次见到会话照原样）。
/// - `model`：`measured` 与 `estimated` 累计的是哪个模型的调用（输出 token）。
#[derive(Clone, Debug, PartialEq)]
pub struct LiveSpeed {
    pub shown: Option<f64>,
    pub was: String,
    pub changed_at: i64,
    pub ink: Ink,
    pub ink_from: Ink,
    pub ink_at: i64,
    pub call: Option<Call>,
    pub seen: Written,
    pub base: Written,
    pub model_turn: bool,
    pub last_key: Option<String>,
    pub primed: bool,
    pub model: String,
    pub measured: f64,
    pub estimated: f64,
}

impl Default for LiveSpeed {
    fn default() -> LiveSpeed {
        LiveSpeed {
            shown: None,
            was: String::new(),
            changed_at: LONG_AGO,
            ink: Ink::Idle,
            ink_from: Ink::Idle,
            ink_at: LONG_AGO,
            call: None,
            seen: Written::default(),
            base: Written::default(),
            model_turn: false,
            last_key: None,
            primed: false,
            model: String::new(),
            measured: 0.0,
            estimated: 0.0,
        }
    }
}

impl LiveSpeed {
    /// 校准：这个模型结束了的调用量到的输出 ÷ 估算的输出；没有时为 1。
    pub fn scale(&self) -> f64 {
        if self.measured > 0.0 && self.estimated > 0.0 {
            self.measured / self.estimated
        } else {
            1.0
        }
    }

    /// 在 `now_ms` 听说了会话的用量：`model` 是在用的模型，`running` 是会话在跑回合。
    pub fn heard(
        &self,
        live: &Heard,
        model: &str,
        running: bool,
        now_ms: f64,
        clock: i64,
    ) -> LiveSpeed {
        let now = Written {
            chars: live.chars as f64,
            wide: live.wide as f64,
        };
        let last_key = live.last_call.as_ref().map(|c| c.0.clone());
        let last_output = live.last_call.as_ref().map_or(0.0, |c| c.1 as f64);
        if !self.primed {
            let mut first = LiveSpeed {
                shown: live.speed_last,
                seen: now,
                base: now,
                model_turn: live.model_turn,
                last_key,
                primed: true,
                model: model.to_owned(),
                ..LiveSpeed::default()
            };
            let ink = first.target(now_ms);
            first.ink = ink;
            first.ink_from = ink;
            return first;
        }
        // 换模型：校准重新开始。
        let same = if model == self.model {
            self.clone()
        } else {
            LiveSpeed {
                model: model.to_owned(),
                measured: 0.0,
                estimated: 0.0,
                ..self.clone()
            }
        };
        let counted = if last_key.is_some() && last_key != self.last_key {
            // 调用结束：它写的字与量到的输出校准估算；下一次调用从这里数起。
            let est = tokens(now.minus(same.base));
            let mut calibrated = same;
            if est > 0.0 && last_output > 0.0 {
                calibrated.measured += last_output;
                calibrated.estimated += est;
            }
            LiveSpeed {
                last_key,
                base: now,
                call: None,
                ..calibrated
            }
        } else {
            // 字变少了：新的回合，从头数起。
            let turn = if now.chars < self.seen.chars {
                LiveSpeed {
                    base: Written::default(),
                    call: None,
                    seen: Written::default(),
                    ..same
                }
            } else {
                same
            };
            if now.chars > turn.seen.chars {
                let call = Some(turn.call.map_or(
                    Call {
                        start_ms: now_ms,
                        last_ms: now_ms,
                    },
                    |c| Call {
                        last_ms: now_ms,
                        ..c
                    },
                ));
                LiveSpeed { call, ..turn }
            } else {
                turn
            }
        };
        // 回合没有等到调用的结束就停了（停止、失败）：结束的调用不显示估算的数。
        let open = if running {
            counted
        } else {
            LiveSpeed {
                call: None,
                ..counted
            }
        };
        let told = LiveSpeed {
            seen: now,
            model_turn: live.model_turn,
            ..open
        };
        let said = if told.call.is_none() {
            told.said(live.speed_last, clock)
        } else {
            told
        };
        said.inked(now_ms, clock)
    }

    /// 计时器的一个 tick：每 [`REFRESH_TICKS`] 重看一次现场的数。
    pub fn tick(&self, now_ms: f64, clock: i64) -> LiveSpeed {
        let refreshed = match self.call {
            Some(c)
                if self.model_turn
                    && clock.rem_euclid(REFRESH_TICKS) == 0
                    && writing_call(c, now_ms) =>
            {
                let rate = self.scale() * tokens(self.seen.minus(self.base))
                    / ((now_ms - c.start_ms) / 1000.0);
                // 差得不够多时不换：末位不闪。
                if self
                    .shown
                    .is_none_or(|v| (rate - v).abs() >= (STEP * v).max(1.0))
                {
                    self.said(Some(rate), clock)
                } else {
                    self.clone()
                }
            }
            _ => self.clone(),
        };
        refreshed.inked(now_ms, clock)
    }

    /// 从 tick `clock` 起说 `v`；变了的位从那时升起。
    pub fn said(&self, v: Option<f64>, clock: i64) -> LiveSpeed {
        let before = self.shown.map(rate).unwrap_or_default();
        if v.map(rate).unwrap_or_default() == before {
            LiveSpeed {
                shown: v,
                ..self.clone()
            }
        } else {
            LiveSpeed {
                shown: v,
                was: before,
                changed_at: clock,
                ..self.clone()
            }
        }
    }

    /// 数与它的单位，在 tick `clock`；什么都不显示时为空。`quiet`：子代理的一列（§4.2「子代理的数字」）：单位安静，模型不在写
    /// 时数是 fg3。
    pub fn spans(&self, theme: &Theme, clock: i64, quiet: bool) -> Vec<Span> {
        let Some(v) = self.shown else {
            return Vec::new();
        };
        let text = rate(v);
        let style_of = |i: Ink| {
            if quiet && i == Ink::Idle {
                theme.fg3
            } else {
                ink_style(theme, i)
            }
        };
        let number = fade::ink(
            theme,
            style_of(self.ink_from).fg_color(),
            style_of(self.ink),
            clock - self.ink_at,
            fade::TICKS,
        );
        let age = clock - self.changed_at;
        // 按右对齐比较：只有变了的位按流式的字的色调出现。数与 `was` 都是 ASCII。
        let chars: Vec<char> = text.chars().collect();
        let was: Vec<char> = self.was.chars().collect();
        let old: Vec<char> = (0..chars.len())
            .map(|i| {
                let k = (was.len() + i).checked_sub(chars.len());
                k.and_then(|k| was.get(k)).copied().unwrap_or(' ')
            })
            .collect();
        let own = match number.fg_color() {
            Color::Default => theme.fg.map(Rgb::color),
            c => Some(c),
        };
        let mut digits: Vec<Span> = Vec::new();
        for (i, ch) in chars.iter().enumerate() {
            let rising = (0..playhead::FADE_TICKS).contains(&age) && old[i] != *ch;
            let st = if rising {
                own.and_then(|c| theme.tint(c, age as f64 / playhead::FADE_TICKS as f64))
                    .map_or(number, |c| number.with_fg(c))
            } else {
                number
            };
            match digits.last_mut() {
                Some(last) if last.style == st => last.text.to_mut().push(*ch),
                _ => digits.push(Span::new(ch.to_string(), st)),
            }
        }
        digits.push(Span::new(
            " tok/s",
            if quiet { theme.quiet } else { theme.fg3 },
        ));
        digits
    }

    /// 数亮着：它的模型在写。
    pub fn writing(&self) -> bool {
        self.shown.is_some() && self.ink == Ink::Lit
    }

    /// 位在升起、墨色在变，或调用在写：计时器要跑。
    pub fn moving(&self, clock: i64) -> bool {
        clock - self.changed_at < playhead::FADE_TICKS
            || clock - self.ink_at < fade::TICKS
            || (self.call.is_some() && self.model_turn)
    }

    fn target(&self, now_ms: f64) -> Ink {
        if !self.model_turn {
            Ink::Idle
        } else if self.call.is_some_and(|c| writing_call(c, now_ms)) {
            Ink::Lit
        } else {
            Ink::Dim
        }
    }

    fn inked(self, now_ms: f64, clock: i64) -> LiveSpeed {
        let to = self.target(now_ms);
        if to == self.ink {
            self
        } else {
            LiveSpeed {
                ink: to,
                ink_from: self.ink,
                ink_at: clock,
                ..self
            }
        }
    }
}

/// 现场的数可以说：调用的第一个字之后 0.5 s，最新一个字之后不到 1 s。
fn writing_call(c: Call, now_ms: f64) -> bool {
    now_ms - c.start_ms >= SHOW_AFTER_MS && now_ms - c.last_ms < HOLD_MS
}
