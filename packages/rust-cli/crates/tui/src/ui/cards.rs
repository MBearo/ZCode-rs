//! 输入层的卡（docs/specs/rust-cli/tui-kit.md §4.2「会话的数字（/stats）」「目标（/goal）」；Puffin `shell/layer_cards.rs`、
//! `stats.rs`、`goals.rs`）：`/stats`（别名 `/status`、`/context`）与 `/goal` 打开的卡在输入框的板上，草稿空着时开着；打字
//! 关上它、字进草稿，esc 关上；目标的卡 ⏎ 暂停或继续。卡的行是 kit 的 `stats::card` 与 `goal::card`。
//!
//! 数字的来源（Puffin 投影的 `usage`）：上下文与速度是会话 actor 给的（`view::Transcript::context_usage`、`output_speed`），
//! 缓存命中率是 runner 查的请求统计（`App::request_stats`），token、回合数与时长是视图折出来的（`view/extras.rs`）。
//! 目标是 runner 从会话取的 `GoalView`，按 Puffin host 的 `goal_of` 换成 kit 的 [`SessionGoal`]。
use std::collections::BTreeMap;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use rust_cli_kernel_api::goal::GoalView;
use rust_cli_tui_kit::app::element::stack;
use rust_cli_tui_kit::app::layout::Layout;
use rust_cli_tui_kit::app::widgets::goal::{
    self as kit_goal, GoalRound, GoalState, GoalVerdict, Parts,
};
use rust_cli_tui_kit::app::widgets::playhead::TICK_MS;
use rust_cli_tui_kit::app::widgets::stats::{self, METER_CELLS};
use rust_cli_tui_kit::app::widgets::{fade, fit, key_hints};
use rust_cli_tui_kit::markdown::render::SETTLED;
use rust_cli_tui_kit::render::{Span, StyledLine};
use rust_cli_tui_kit::theme::theme::Theme;

pub use rust_cli_tui_kit::app::widgets::goal::SessionGoal;

use super::app::{Action, App, Overlay};
use super::stats_page::Tab;

/// 开着的卡。
#[derive(Debug, Clone, PartialEq)]
pub enum Card {
    /// 会话的数字；`Figures` 记每个数什么时候变的（淡入）。
    Stats(Figures),
    /// 目标；没有目标时为 `None`（卡说怎样设一个）。
    Goal(Option<SessionGoal>),
    /// `/usage` 的卡：第几个条目（usage_plans.rs）。
    Usage { entry: usize },
    /// `/usage` 的详情页（代替正文，`page_frame.rs`）：条目、范围、顶行。
    UsagePage {
        entry: usize,
        range: usize,
        top: usize,
    },
    /// 统计的页（数字的卡上 ⏎ 或 ⇥，代替正文；Context 与 Session 两个标签，stats_page.rs，tui-panels.md §4）。
    StatsPage(super::stats_page::StatsPage),
}

/// 卡上每个淡入的部分现在说的（只用来比较）与它变的那一帧（帧的 `now_ms` 与时刻）；打开之前就是这样的算落定了（Puffin `figures_at`
/// 缺省 `LONG_AGO`）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Figures {
    said: BTreeMap<&'static str, String>,
    at: BTreeMap<&'static str, (u64, Instant)>,
}

/// `GoalView` → kit 的目标（Puffin host `goal_of`）：在跑的目标是第几轮，结束了的是它的结论。
pub fn goal_of(view: &GoalView, now_ms: u64) -> Option<SessionGoal> {
    let rounds = |round: u32, step: &str, last: GoalVerdict| {
        (1..=round)
            .map(|r| GoalRound {
                round: r,
                step: if r == 1 {
                    step.to_owned()
                } else {
                    String::new()
                },
                verdict: if r < round { GoalVerdict::NotYet } else { last },
                done: 0,
                total: 0,
            })
            .collect()
    };
    if let Some(active) = &view.active {
        let round = active.iterations + 1;
        return Some(SessionGoal {
            target_id: String::new(),
            objective: active.condition.clone(),
            title: String::new(),
            status: "active".to_owned(),
            state: GoalState::Working,
            round,
            time_used_seconds: now_ms.saturating_sub(active.set_at_ms) / 1000,
            running_since: 0,
            rounds: rounds(round, &active.condition, GoalVerdict::None),
            reason: active.last_reason.clone().unwrap_or_default(),
        });
    }
    let done = view.achieved.as_ref()?;
    let round = done.iterations.unwrap_or(1).max(1);
    let (state, status, last) = if done.failed {
        (GoalState::Failed, "failed", GoalVerdict::Failed)
    } else if done.met {
        (GoalState::Met, "verified", GoalVerdict::Met)
    } else {
        (GoalState::Stalled, "notSatisfied", GoalVerdict::Stalled)
    };
    Some(SessionGoal {
        target_id: String::new(),
        objective: done.condition.clone(),
        title: String::new(),
        status: status.to_owned(),
        state,
        round,
        time_used_seconds: done.duration_ms.unwrap_or(0) / 1000,
        running_since: 0,
        rounds: rounds(round, &done.condition, last),
        reason: done.reason.clone().unwrap_or_default(),
    })
}

/// 结束了的子代理的合计（Puffin `subagents::ended`）：主会话里结束了的 Agent 调用的个数、报告的 token、用时（报告的
/// `duration_ms`，没有时是开始到结束）与查来的平均速度。
fn ended(view: &crate::view::Transcript) -> stats::Ended {
    use rust_cli_tui_kit::app::widgets::tools;
    let mut e = stats::Ended::default();
    for row in view.rows() {
        let crate::view::Body::Tool(tool) = &row.body else {
            continue;
        };
        let owned = super::cells::calls::owned(row, tool);
        let c = super::cells::calls::call(tool, &owned);
        if !(tools::subagent(&c) && c.ended) {
            continue;
        }
        e.count += 1;
        e.tokens += tools::reported(&c, "subagent_tokens").unwrap_or(0.0);
        let (s, f) = tools::times(&c);
        e.work_ms += tools::reported(&c, "duration_ms")
            .or_else(|| (s > 0.0 && f > s).then_some(f - s))
            .unwrap_or(0.0);
        if let Some(avg) = view.figures(tool.id.as_str()).and_then(|x| x.avg) {
            e.speeds.push(avg);
        }
    }
    e
}

/// 每个淡入的部分说的（Puffin `stats::said`）。
fn said(u: &stats::Usage, e: &stats::Ended) -> BTreeMap<&'static str, String> {
    let context = u.context.as_ref().map_or(String::new(), |c| {
        format!(
            "{}|{}|{}|{}|{:?}",
            stats::percent(c),
            stats::filled(c, METER_CELLS),
            fit::count(c.used as f64),
            fit::count(c.max as f64),
            c.breakdown
        )
    });
    let speed = format!(
        "{}|{:?}",
        u.last_speed.map(fit::rate).unwrap_or_default(),
        u.speeds
    );
    let tokens = format!(
        "{}|{}",
        fit::count(u.input as f64),
        fit::count(u.output as f64)
    );
    let cache = u.cache.map_or(String::new(), |c| {
        format!(
            "{}|{}",
            stats::share(c.hit_rate),
            c.last_call.map(stats::share).unwrap_or_default()
        )
    });
    let subagents = if e.count == 0 {
        String::new()
    } else {
        let mut speeds = e.speeds.clone();
        speeds.sort_by(f64::total_cmp);
        format!(
            "{}|{}|{}|{speeds:?}",
            e.count,
            fit::count(e.tokens),
            stats::working(e.work_ms)
        )
    };
    let inall = if e.count == 0 {
        String::new()
    } else {
        fit::count(u.input as f64 + u.output as f64 + e.tokens)
    };
    BTreeMap::from([
        ("context", context),
        ("speed", speed),
        ("tokens", tokens),
        ("cache", cache),
        ("subagents", subagents),
        ("inall", inall),
    ])
}

impl App {
    /// 卡上的数（Puffin `kit_usage`）：看的是现在显示的会话（查看子代理时是它的）。
    pub fn stats_usage(&self) -> stats::Usage {
        let shown = self.shown();
        let (input, output) = shown.usage().values().fold((0, 0), |(i, o), u| {
            (
                i + u.input_tokens + u.cache_creation_input_tokens + u.cache_read_input_tokens,
                o + u.output_tokens,
            )
        });
        let (turns, active_ms) = shown.turn_totals();
        stats::Usage {
            // 用掉的与窗口是会话视图的；组成来自上下文的计数（tui-panels.md §4.1），还没有答案时空着、卡没有图例。视图还没有
            // 用量（第一条消息之前）时用计数的窗口。
            context: shown
                .context_usage()
                .map(|c| stats::Window {
                    used: c.used_tokens,
                    max: c.max_tokens,
                    compact_at: c.compact_at,
                    breakdown: self.context_breakdown(),
                })
                .or_else(|| {
                    self.context_now()
                        .filter(|u| u.raw_max_tokens > 0)
                        .map(super::context_page::window_of)
                }),
            cache: self
                .snapshots
                .request_stats
                .stats()
                .cache_hit_rate()
                .map(|hit_rate| stats::Cache {
                    hit_rate,
                    last_call: None,
                }),
            input,
            output,
            // 最近一次：请求统计量到的（同 Puffin），还没量到时是会话 actor 给的。
            last_speed: self
                .snapshots
                .request_stats
                .speeds()
                .last
                .or_else(|| shown.output_speed().and_then(|s| s.tokens_per_second())),
            speeds: self.snapshots.request_stats.speeds().turns.clone(),
            last_call_tokens: shown.last_call_tokens(),
            turns,
            active_ms,
            complete: false,
        }
    }

    /// 卡上结束了的子代理（Puffin `stats_more`）：看的是现在显示的会话。
    pub fn stats_ended(&self) -> stats::Ended {
        ended(self.shown())
    }

    /// `/stats`（`/status`、`/context`）：草稿清空，打开数字的卡（Puffin 的 `"stats"` 分支）；页上 esc 也回到这里。
    /// runner 接着问上下文的计数（卡的组成，tui-panels.md §4.1）。
    pub fn open_stats(&mut self) -> Action {
        self.composer.clear();
        self.overlay = Some(Overlay::Card(Card::Stats(Figures {
            said: said(&self.stats_usage(), &self.stats_ended()),
            at: BTreeMap::new(),
        })));
        Action::CountContext
    }

    /// 从会话取到的目标：记作会话的目标（状态栏），卡开着时换上；`opening`（`/goal` 刚发出）时打开卡，取的时候卡已经关上
    /// （打了字、开了别的）就不开。
    pub fn open_goal_card(&mut self, view: &GoalView, now_ms: u64, opening: bool) {
        self.show_goal_card(goal_of(view, now_ms), opening);
    }

    /// 会话的目标是 `goal`（[`Self::open_goal_card`] 的后半；孪生门禁直接交 kit 的目标）。
    pub fn show_goal_card(&mut self, goal: Option<SessionGoal>, opening: bool) {
        self.session_goal = goal.clone();
        match &mut self.overlay {
            Some(Overlay::Card(Card::Goal(shown))) => *shown = goal,
            None if opening && self.composer.is_empty() => {
                self.overlay = Some(Overlay::Card(Card::Goal(goal)));
            }
            _ => {}
        }
    }

    /// 发送：达成的目标的字从状态栏消失（Puffin 发送时的 `goal_met_seen`）。
    pub(super) fn goal_sent(&mut self) {
        self.goal_met_seen = self
            .session_goal
            .as_ref()
            .filter(|g| g.state == GoalState::Met)
            .map(kit_goal::met_key);
    }

    /// 状态栏模式后面目标的记号与字（kit `goal::status_parts`，Puffin `status_line.rs`）；没有目标时为 None。
    pub fn goal_status_parts(&self) -> Option<(Parts, Parts)> {
        let goal = self.session_goal.as_ref()?;
        let seen = self.goal_met_seen.as_deref() == Some(kit_goal::met_key(goal).as_str());
        Some(kit_goal::status_parts(&self.ink.theme, goal, seen))
    }

    /// 每一帧：数字的卡记下变了的数（淡入从那一帧起，Puffin `post.rs` 的 `figures_at`）。
    pub fn cards_frame(&mut self, now: Instant, now_ms: u64) {
        if !matches!(self.overlay, Some(Overlay::Card(Card::Stats(_)))) {
            return;
        }
        let says = said(&self.stats_usage(), &self.stats_ended());
        if let Some(Overlay::Card(Card::Stats(figures))) = self.overlay.as_mut() {
            for (key, value) in says {
                if !value.is_empty() && figures.said.get(key) != Some(&value) {
                    figures.at.insert(key, (now_ms, now));
                }
                figures.said.insert(key, value);
            }
        }
    }

    /// 卡在淡入：按动画节拍重画。
    pub fn cards_fading(&self) -> bool {
        if self.usage_moving() || self.usage_back_moving() {
            return true;
        }
        let ticks = u128::from(TICK_MS) * u128::from(fade::TICKS.unsigned_abs());
        match &self.overlay {
            Some(Overlay::Card(Card::StatsPage(_))) => self.context_fading(),
            Some(Overlay::Card(Card::Stats(figures))) => figures
                .at
                .values()
                .any(|(_, at)| at.elapsed().as_millis() < ticks),
            // 目标的卡开着、目标正在跑的这一段有开始时刻：用时每帧在走（Puffin `goal_moving` 的 `running_since > 0`；
            // 两边的 host 现在都给 0，内核给出这一段的开始时刻时才动）。
            Some(Overlay::Card(Card::Goal(Some(goal)))) => goal.running_since > 0,
            _ => false,
        }
    }

    /// 卡拿的键（Puffin `goal_key`、`stats_key`）：esc 关上；目标的卡 ⏎ 暂停或继续（`/goal pause` / `/goal resume`
    /// 发给内核，同 Puffin host 的 `PauseGoal` / `ResumeGoal`）；其余的键去编辑草稿，打了字卡就关上（[`Self::settle_card`]）。
    pub(super) fn card_key(&mut self, key: KeyEvent) -> Option<Option<Action>> {
        let Some(Overlay::Card(card)) = &self.overlay else {
            return None;
        };
        match *card {
            Card::Usage { entry } => return self.usage_card_key(key, entry),
            Card::UsagePage { entry, range, top } => {
                return self.usage_page_key(key, (entry, range, top));
            }
            Card::StatsPage(_) => return self.stats_page_key(key),
            Card::Stats(_) | Card::Goal(_) => {}
        }
        match key.code {
            KeyCode::Esc => {
                self.overlay = None;
                Some(None)
            }
            KeyCode::Enter => match card {
                Card::Goal(goal) => {
                    Some(
                        kit_goal::enter_does(goal.as_ref()).map(|does| Action::Submit {
                            text: format!("/goal {does}"),
                            delivery: rust_cli_kernel_api::session_api::Delivery::Queue,
                        }),
                    )
                }
                // 数字的卡上 ⏎ 打开页的 Context 标签（tui-panels.md §4.1）。
                Card::Stats(_) => Some(self.open_stats_page(Tab::Context)),
                Card::Usage { .. } | Card::UsagePage { .. } | Card::StatsPage(_) => Some(None),
            },
            // 数字的卡上 ⇥ 打开页的 Session 标签（§4.1）。
            KeyCode::Tab if matches!(card, Card::Stats(_)) => {
                Some(self.open_stats_page(Tab::Session))
            }
            _ => None,
        }
    }

    /// 一个键之后：草稿里有字了，卡关上（Puffin `settle_layer_card`）。
    pub(super) fn settle_card(&mut self) {
        if matches!(self.overlay, Some(Overlay::Card(_))) && !self.composer.is_empty() {
            self.overlay = None;
        }
    }

    /// 卡的行与槽（`pane.rs` 放在板上）；有请求在等时不画（Puffin `layer_card_open` 的 `!asking`）。
    /// `tick` 是这一帧的动画节拍（`/usage` 的卡按它淡入，usage_plans.rs）。
    pub fn card_layer(
        &self,
        width: usize,
        tick: i64,
        now_ms: u64,
    ) -> Option<(Vec<StyledLine>, Vec<Span>)> {
        let Some(Overlay::Card(card)) = &self.overlay else {
            return None;
        };
        if self.asking() {
            return None;
        }
        let theme: &Theme = &self.ink.theme;
        let (rows, keys) = match card {
            Card::Stats(figures) => {
                let age = |key: &str| {
                    figures.at.get(key).map_or(SETTLED, |(at, _)| {
                        i64::try_from(now_ms.saturating_sub(*at) / TICK_MS).unwrap_or(SETTLED)
                    })
                };
                let rows = stats::card(
                    theme,
                    self.shown().title(),
                    &self.stats_usage(),
                    &age,
                    &self.stats_ended(),
                );
                // 槽是 `⏎ context  ⇥ session`（Puffin 是空的）；查看子代理时没有 Context（stats_page.rs）。
                let mut keys = Vec::new();
                if self.viewing.is_none() {
                    keys.push(("⏎".to_owned(), "context".to_owned()));
                }
                keys.push(("⇥".to_owned(), "session".to_owned()));
                (rows, keys)
            }
            Card::Goal(goal) => (
                kit_goal::card(theme, goal.as_ref(), now_ms as f64),
                kit_goal::card_keys(goal.as_ref()),
            ),
            Card::Usage { entry } => {
                return Some(self.usage_card_layer(*entry, width, tick, now_ms));
            }
            // 详情页与上下文的页不在板上：它们代替正文（`page_frame.rs`）。
            Card::UsagePage { .. } | Card::StatsPage(_) => return None,
        };
        Some((
            Layout::lay(&stack(rows), width),
            key_hints::spans(theme, &key_hints::pick(&keys)),
        ))
    }
}

#[cfg(test)]
#[path = "cards_tests.rs"]
mod tests;
