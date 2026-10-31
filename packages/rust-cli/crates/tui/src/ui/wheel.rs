//! 滚轮（修复线 F25，docs/specs/rust-cli/tui-select.md §4）：唯一的入口 [`App::wheel`] 按最上层的可滚动视图分发，
//! 没有时滚 transcript；每格滚几行按加速的规则算。
//!
//! 缺陷修复（F25）：原来滚轮按视图种类分散在 `on_mouse` 里，独立页面（`/help`、`/skills`、`/mcp`、`/status`、`/context`）、
//! `/usage`、`/permissions`、`/diff` 没有分支，滚轮落到被盖住的 transcript；会话面板的分支处理完又往下落，同时滚了
//! transcript。现在只有这一个分发点。
use std::collections::BTreeMap;
use std::time::Instant;

use rust_cli_base::units::MS_PER_SECOND_F64;

use super::app::{App, Overlay};

/// 两格之间多久算连续。
const BURST_MS: f64 = 40.0;
/// 换方向后的第二格在这之内才算鼠标滚轮。
const FLIP_MS: f64 = 200.0;
/// 滚轮模式多久没有滚动就退出。
const WHEEL_IDLE_MS: f64 = 1500.0;
/// 衰减的半衰期。
const HALF_LIFE_MS: f64 = 150.0;
/// 连发。
const REPEAT_MS: f64 = 5.0;
/// 半衰期的底数：每过一个半衰期剩一半。
const HALF: f64 = 0.5;
/// 每格的基础行数：JetBrains 的终端 2 行；VS Code、Windows、Windows Terminal 3 行；`ZCODE_SCROLL_SPEED` 不超过 20。
const JETBRAINS_ROWS: f64 = 2.0;
const FAST_ROWS: f64 = 3.0;
const MAX_SCROLL_SPEED: f64 = 20.0;
/// 每格发很多个事件的终端：两格之间超过 [`BURST_MS`] 时滚基础行数的 3 倍。
const FLOOD_GAP_MULT: f64 = 3.0;
/// 滚轮模式里连发这么多次就退出滚轮模式。
const REPEAT_BURST: u32 = 5;
/// 滚轮模式的加速：倍数的上限（基础行数不到 1 时按比例缩小，至少基础的 2 倍）、每格叠加的量、每格最多加多少。
const WHEEL_MAX_MULT: f64 = 15.0;
const WHEEL_BOOST: f64 = 15.0;
const WHEEL_MAX_STEP: f64 = 3.0;
/// 连续滚动（不在滚轮模式）：倍数的上限与每格加的量。
const BURST_MAX_MULT: f64 = 6.0;
const BURST_STEP: f64 = 0.3;
/// 衰减模式（VS Code、Windows）：停顿超过 500 ms 或换方向时从基础的、至少 2 倍起；两格间隔不小于 80 ms 时倍数
/// 不超过 3，否则不超过 36；每格叠加的量。
const DECAY_RESET_MS: f64 = 500.0;
const DECAY_MIN_MULT: f64 = 2.0;
const SLOW_GAP_MS: f64 = 80.0;
const SLOW_MAX_MULT: f64 = 3.0;
const FAST_MAX_MULT: f64 = 36.0;
const DECAY_BOOST: f64 = 7.0;
/// 终端版本号编码成一个数：主版本 × 1000000 + 次版本 × 1000 + 修订号。
const VERSION_MAJOR: u64 = 1_000_000;
const VERSION_MINOR: u64 = 1_000;
/// 每格发很多个滚轮事件的 VS Code 版本：1.92 到 1.104。
const FLOOD_VERSIONS: std::ops::Range<u64> = 1_092_000..1_105_000;

/// 终端事实与加速的状态。
#[derive(Debug, Clone, PartialEq)]
pub struct Accel {
    base: f64,
    decay: bool,
    flood: bool,
    /// 上一格的时刻（毫秒，相对 `origin`）。
    time: f64,
    mult: f64,
    dir: i8,
    frac: f64,
    pending_flip: bool,
    wheel_mode: bool,
    burst: u32,
    origin: Option<Instant>,
}

impl Default for Accel {
    fn default() -> Self {
        Self::new(&BTreeMap::new(), false)
    }
}

/// `1.92.0` → 1092000。
fn version(text: Option<&String>) -> Option<u64> {
    let mut parts = text?.split(['.', '-', '+']);
    let mut next = || parts.next()?.parse::<u64>().ok();
    Some(next()? * VERSION_MAJOR + next()? * VERSION_MINOR + next()?)
}

impl Accel {
    /// 按终端变量取参数。`windows`：本工程在 Windows 上运行。
    pub fn new(vars: &BTreeMap<String, String>, windows: bool) -> Self {
        let get = |name: &str| vars.get(name).filter(|v| !v.is_empty());
        let vscode = get(rust_cli_base::names::env::TERM_PROGRAM).is_some_and(|p| p == "vscode");
        // Cursor 与 VS Code 1.92–1.104 每格发很多个滚轮事件。
        let flood = get(rust_cli_base::names::env::CURSOR_TRACE_ID).is_some()
            || get(rust_cli_base::names::env::VSCODE_GIT_ASKPASS_MAIN)
                .is_some_and(|v| v.contains("cursor"))
            || (vscode
                && version(get(rust_cli_base::names::env::TERM_PROGRAM_VERSION))
                    .is_some_and(|v| FLOOD_VERSIONS.contains(&v)));
        let jetbrains = get(rust_cli_base::names::env::TERMINAL_EMULATOR)
            .is_some_and(|t| t == "JetBrains-JediTerm");
        let wt = get(rust_cli_base::names::env::WT_SESSION).is_some();
        let fast = !flood && (vscode || windows || wt);
        let base = if jetbrains {
            JETBRAINS_ROWS
        } else {
            let fallback = if fast { FAST_ROWS } else { 1.0 };
            // `ZCODE_SCROLL_SPEED`（修复线 F42 改名，config.md §2.5）。
            get(rust_cli_base::names::env::ZCODE_SCROLL_SPEED)
                .and_then(|v| v.trim().parse::<f64>().ok())
                .filter(|v| *v > 0.0)
                .map_or(fallback, |v| v.min(MAX_SCROLL_SPEED))
        };
        Self {
            base,
            decay: !flood && (vscode || windows || wt),
            flood,
            time: 0.0,
            mult: base,
            dir: 0,
            frac: 0.0,
            pending_flip: false,
            wheel_mode: false,
            burst: 0,
            origin: None,
        }
    }

    /// 一格滚轮滚几行（没有 JetBrains 触控板分支）。`down`：向下。
    pub fn rows(&mut self, down: bool, now: Instant) -> usize {
        let origin = *self.origin.get_or_insert(now);
        let at = now.duration_since(origin).as_secs_f64() * MS_PER_SECOND_F64;
        self.rows_at(if down { 1 } else { -1 }, at)
    }

    /// 同 [`Accel::rows`]，时刻直接给出（毫秒，单测用）。
    pub fn rows_at(&mut self, k: i8, at: f64) -> usize {
        let floor = |v: f64| v.floor().max(1.0) as usize;
        if !self.decay {
            if self.flood {
                let gap = at - self.time;
                self.time = at;
                self.dir = k;
                self.mult = if gap > BURST_MS {
                    self.base * FLOOD_GAP_MULT
                } else {
                    self.base
                };
                return floor(self.mult);
            }
            if self.wheel_mode && at - self.time > WHEEL_IDLE_MS {
                self.wheel_mode = false;
                self.burst = 0;
                self.mult = self.base;
            }
            if self.pending_flip {
                self.pending_flip = false;
                if k != self.dir || at - self.time > FLIP_MS {
                    self.dir = k;
                    self.time = at;
                    self.mult = self.base;
                    return floor(self.mult);
                }
                self.wheel_mode = true;
            }
            let gap = at - self.time;
            if k != self.dir && self.dir != 0 {
                self.pending_flip = true;
                self.time = at;
                return 0;
            }
            self.dir = k;
            self.time = at;
            if self.wheel_mode {
                if gap < REPEAT_MS {
                    self.burst += 1;
                    if self.burst >= REPEAT_BURST {
                        self.wheel_mode = false;
                        self.burst = 0;
                        self.mult = self.base;
                    } else {
                        return 1;
                    }
                } else {
                    self.burst = 0;
                }
            }
            if self.wheel_mode {
                let decay = HALF.powf(gap / HALF_LIFE_MS);
                let cap = (WHEEL_MAX_MULT * self.base.min(1.0)).max(self.base * 2.0);
                let next = 1.0 + (self.mult - 1.0) * decay + WHEEL_BOOST * decay;
                self.mult = cap.min(next).min(self.mult + WHEEL_MAX_STEP);
                return floor(self.mult);
            }
            if gap > BURST_MS {
                self.mult = self.base;
            } else {
                let cap = (BURST_MAX_MULT * self.base.min(1.0)).max(self.base * 2.0);
                self.mult = cap.min(self.mult + BURST_STEP);
            }
            return floor(self.mult);
        }
        let gap = at - self.time;
        let same = k == self.dir;
        self.time = at;
        self.dir = k;
        if same && gap < REPEAT_MS {
            return 1;
        }
        if !same || gap > DECAY_RESET_MS {
            self.mult = self.base.max(DECAY_MIN_MULT);
            self.frac = 0.0;
        } else {
            let decay = HALF.powf(gap / HALF_LIFE_MS);
            let cap: f64 = if gap >= SLOW_GAP_MS {
                SLOW_MAX_MULT
            } else {
                FAST_MAX_MULT
            };
            self.mult = cap.min(1.0 + (self.mult - 1.0) * decay + DECAY_BOOST * decay);
        }
        let total = self.mult + self.frac;
        let rows = total.floor();
        self.frac = total - rows;
        rows as usize
    }
}

impl App {
    /// 一格滚轮：按最上层的可滚动视图分发（tui-select.md §4.1）。列表类每格 1 项，滚动类按加速的行数。
    pub fn wheel(&mut self, down: bool, now: Instant) {
        let step: isize = if down { 1 } else { -1 };
        let rows = self.wheel.rows(down, now) as isize;
        let lines = if down { rows } else { -rows };
        // 全屏视图的选区按屏幕坐标记：内容滚动之后不再对应原来的文字（tui-select.md D3）。
        if self.overlay.as_ref().is_some_and(super::mouse::full_screen) {
            self.screen_selection = None;
        }
        match &self.overlay {
            // 详细视图的滚动量是离底部的行数。
            Some(Overlay::Detail(_)) => self.detail_wheel(-lines),
            Some(Overlay::Page { .. }) => {
                if let Some(Overlay::Page { scroll, .. }) = self.overlay.as_mut() {
                    *scroll = scroll.saturating_add_signed(lines);
                }
            }
            Some(Overlay::Usage(_)) => {
                if let Some(Overlay::Usage(view)) = self.overlay.as_mut() {
                    view.wheel(lines);
                }
            }
            Some(Overlay::Diff(_)) => self.diff_wheel(lines),
            Some(Overlay::List(_)) => {
                if let Some(Overlay::List(list)) = self.overlay.as_mut() {
                    list.wheel(step);
                }
            }
            Some(Overlay::Permissions(_)) => {
                if let Some(Overlay::Permissions(view)) = self.overlay.as_mut() {
                    view.wheel(step);
                }
            }
            Some(Overlay::Projects(_)) => self.projects_wheel(step),
            Some(Overlay::Resume(_)) => self.resume_wheel(step),
            Some(Overlay::Trust(_)) => {}
            Some(Overlay::Background(_)) => self.background_wheel(step, now),
            Some(
                Overlay::Login(_)
                | Overlay::Export(_)
                | Overlay::Shortcuts
                | Overlay::YoloConfirm(_)
                | Overlay::Machines(_),
            )
            | None => {
                self.scroll_by(-lines);
            }
        }
    }
}

#[cfg(test)]
#[path = "wheel_tests.rs"]
mod tests;
