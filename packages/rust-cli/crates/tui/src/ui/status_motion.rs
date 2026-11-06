//! 状态栏的时钟（tui-kit.md §4.2「footer」；照 docs/puffin-tui.md §4.2「状态栏」的「变化」与「多少」的「现场的速度」）：
//! 模式、目标的记号、模型的标签与上下文的数在变了的那个 tick 淡入（Puffin 的 `mode_at`、`goal_at`、`label_at` 与
//! `figures_at["context"]`），速度是 kit `live_speed` 按到达的字估算的现场的数（Puffin `sped`）；模式变了时输入框上方安静的
//! 一句说新的模式（Puffin `asked` 的 `mode_line`）。
//!
//! 数据只有视图与画的时刻：画之前比一次视图（与 `input_motion` 同一个办法，runner 在每批事件与每次输入之后马上画，与 Puffin
//! 在投影到达的那个 tick 比的是同一刻）。tick 是 `App::started` 起走过的动画 tick（孪生里与 Puffin 的 `state.clock` 相同）。
//! 以前状态栏每个部分都当作早已落定，速度只来自内核的 `OutputSpeed`，模式的一句由 `/mode` 在命令发出之前就说了。
use std::time::Instant;

use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_tui_kit::app::widgets::fade::TICKS;
use rust_cli_tui_kit::app::widgets::live_speed::LiveSpeed;
use rust_cli_tui_kit::app::widgets::playhead::TICK_MS;
use rust_cli_tui_kit::app::widgets::stats::{self, METER_CELLS};
use rust_cli_tui_kit::render::Span;

use super::app::App;

/// 很久以前（同 Puffin 状态的 `LONG_AGO`）：从这时起的淡入早已落定。
const LONG_AGO: i64 = -1_000_000;
/// 一帧最多补走的 tick（窗口失去焦点很久之后不必一个个补；速度的刷新只看最近的）。
const CATCH_UP: i64 = 64;

/// 上一帧见到的：会话、模式与计划模式、目标的状态、模型的标签、上下文的数。
#[derive(Debug, Clone, PartialEq)]
struct Seen {
    session: Option<String>,
    mode: (PermissionMode, bool),
    goal: Option<String>,
    label: (String, String),
    context: String,
}

#[derive(Debug)]
pub struct StatusMotion {
    /// 这一帧的 tick；没画过时为 None。
    clock: Option<i64>,
    seen: Option<Seen>,
    mode_at: i64,
    goal_at: i64,
    label_at: i64,
    context_at: i64,
    /// `/model` 刚选的（模型，档位）：状态栏的标签变成它时淡入（Puffin `picked`）。
    pub picked: Option<(String, String)>,
    /// 刚批准了计划：随之关掉计划模式时不说 `Plan mode is off.`（Puffin `plan_approved`，回答请求的地方记下）。
    pub plan_approved: bool,
    /// 恢复的会话到了：标签从下一个 tick 淡入（Puffin `resumed_into` 的 `label_at`，sessions_list.rs）。
    pub label_fresh: bool,
    live: LiveSpeed,
}

impl Default for StatusMotion {
    fn default() -> Self {
        Self {
            clock: None,
            seen: None,
            mode_at: LONG_AGO,
            goal_at: LONG_AGO,
            label_at: LONG_AGO,
            context_at: LONG_AGO,
            picked: None,
            plan_approved: false,
            label_fresh: false,
            live: LiveSpeed::default(),
        }
    }
}

impl StatusMotion {
    fn now(&self) -> i64 {
        self.clock.unwrap_or(0)
    }

    /// 上一帧的 tick（Puffin `state.clock`：动画的节拍数，不是墙上时间）；`/usage` 的回答到达时按它记（usage_plans.rs）。
    pub fn tick(&self) -> i64 {
        self.now()
    }

    pub fn mode_age(&self) -> i64 {
        self.now() - self.mode_at
    }

    pub fn goal_age(&self) -> i64 {
        self.now() - self.goal_at
    }

    pub fn label_age(&self) -> i64 {
        self.now() - self.label_at
    }

    pub fn context_age(&self) -> i64 {
        self.now() - self.context_at
    }

    /// 状态栏的速度（kit `LiveSpeed::spans`）；什么都不说时为空。
    pub fn speed_spans(&self, theme: &rust_cli_tui_kit::theme::theme::Theme) -> Vec<Span> {
        self.live.spans(theme, self.now(), false)
    }

    /// 还有东西在淡入、速度的位在升起或调用在写（按动画节拍重画）。
    pub fn moving(&self) -> bool {
        let now = self.now();
        [self.mode_at, self.goal_at, self.label_at, self.context_at]
            .iter()
            .any(|at| now - at < TICKS)
            || self.live.moving(now)
    }
}

/// 上下文的数说的（Puffin `stats::said` 的 `context`，不含只在 `/stats` 里的组成）。
fn context_said(app: &App) -> String {
    app.view.context_usage().map_or(String::new(), |u| {
        let w = stats::Window {
            used: u.used_tokens,
            max: u.max_tokens,
            compact_at: u.compact_at,
            breakdown: Vec::new(),
        };
        format!(
            "{}|{}|{}|{}",
            stats::percent(&w),
            stats::filled(&w, METER_CELLS),
            rust_cli_tui_kit::app::widgets::fit::count(w.used as f64),
            rust_cli_tui_kit::app::widgets::fit::count(w.max as f64),
        )
    })
}

fn seen(app: &App) -> Seen {
    let selection = app.view.selection();
    Seen {
        session: app.session.clone(),
        mode: (app.view.mode(), app.view.plan()),
        goal: app.session_goal.as_ref().map(|g| format!("{:?}", g.state)),
        label: (
            selection.model.clone(),
            selection.reasoning.clone().unwrap_or_default(),
        ),
        context: context_said(app),
    }
}

/// 画之前：走过的 tick 交给速度，视图听说一次；与上一帧比，变了的部分从这个 tick 淡入，模式变了说一句。
/// `now` 这一帧的 tick（从启动起的动画节拍数）。
pub fn tick_at(app: &App, now: Instant) -> i64 {
    let ms = now.saturating_duration_since(app.started).as_millis();
    i64::try_from(ms / u128::from(TICK_MS)).unwrap_or(i64::MAX / 4)
}

pub fn note(app: &mut App, now: Instant, now_ms: u64) {
    let clock = tick_at(app, now);
    let wall = now_ms as f64;
    let heard = app.view.heard();
    let running = app.view.running();
    let selection = app.view.selection();
    let model = format!("{} {}", selection.provider, selection.model);
    let says = seen(app);
    let m = &mut app.status_motion;
    // 速度：先走过这一帧之前的 tick（Puffin 每个 tick 的 `tick`），再听说视图（Puffin 每次投影的 `observe`）。
    let from = m.clock.map_or(clock, |c| (c + 1).max(clock - CATCH_UP));
    for c in from..=clock {
        m.live = m.live.tick(wall, c);
    }
    m.clock = Some(clock);
    m.live = m.live.heard(&heard, &model, running, wall, clock);
    let Some(was) = m.seen.replace(says.clone()) else {
        return;
    };
    let same = was.session == says.session;
    // 模式变化（Puffin `asked`）：状态栏打出新的模式，输入框上方说它的意思；批准计划关掉计划模式时不说。
    let line = if !same || was.mode == says.mode {
        None
    } else {
        m.mode_at = clock;
        let ((bm, bp), (am, ap)) = (was.mode, says.mode);
        if am != bm {
            Some(super::i18n::mode_says(app.lang, am))
        } else if ap && !bp {
            Some(super::i18n::mode_says(app.lang, PermissionMode::Plan))
        } else if !ap && bp && !m.plan_approved {
            Some(super::i18n::plan_off_says(app.lang))
        } else {
            None
        }
    };
    // 计划模式关掉之后不再记着批准过（下一次关掉照常说）。
    m.plan_approved = m.plan_approved && (!was.mode.1 || says.mode.1);
    // 目标的这一段（Puffin `goal_changed`）：同一个会话里目标出现或换了状态。
    if same && says.goal.is_some() && says.goal != was.goal {
        m.goal_at = clock;
    }
    // 模型的标签变成了 `/model` 选的那个（Puffin `labelled`），或恢复的会话到了；别的变化直接出现。
    if std::mem::take(&mut m.label_fresh) {
        m.label_at = clock;
    }
    if was.label != says.label && m.picked.as_ref() == Some(&says.label) {
        m.label_at = clock;
        m.picked = None;
    }
    // 上下文的数说的变了（Puffin `figured` 的 `context`）。
    if !says.context.is_empty() && says.context != was.context {
        m.context_at = clock;
    }
    if let Some(line) = line {
        app.info(line);
    }
}
