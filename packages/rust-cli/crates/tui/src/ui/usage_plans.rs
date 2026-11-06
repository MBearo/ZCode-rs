//! `/usage`（别名 `/cost`）的卡与详情页（docs/specs/rust-cli/tui-kit.md §4.2「`/usage`」；照 Puffin「用量（/usage）」，
//! `shell/usage/mod.rs`）：卡在输入框的板上（同 `/stats` 的卡，cards.rs），←→ 换计划，⏎ 打开计划的详情页；详情页代替正文
//! （`page_frame.rs`），←→ 换范围，⇥ 换计划，esc 回到卡；打字关上卡与页、字进草稿。卡与页的行是 kit 的
//! `widgets::usage::{view, page}`。
//!
//! 数据只有入口的回答（[`crate::services::PlanUsage`]，与 cli 的 Puffin host 同一个端口与一分钟的缓存）：`UsageUi` 记读到的
//! 计划、额度与历史，问题经 [`Action::PlanUsage`] 交给 runner，回答回到 [`App::plan_answered`]。同一个问题同时只问一次
//! （`asking`）。淡入按动画节拍（`TICK_MS`）记到达的 tick。
//!
//! 失败回合的「回来的时刻」（Puffin `ask_back`）在 usage_back.rs。与 Puffin 的不同（tui-kit.md §4.3 登记）：旧界面的会话
//! 用量与本机的活动统计在 `/usage all`（`usage.rs`）。
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_tui_kit::app::element::{Element, br, measure, stack};
use rust_cli_tui_kit::app::layout::Layout;
use rust_cli_tui_kit::app::widgets::playhead::TICK_MS;
use rust_cli_tui_kit::app::widgets::usage::{
    DEFAULT_RANGE, Entry, Family, PlanDetail, PlanKind, PlanQuota, QuotaState, RANGES, Seen, Usage,
    UsageModel, UsagePlan, UsageRange, page, range, range_name, view,
};
use rust_cli_tui_kit::app::widgets::{fade, key_hints};
use rust_cli_tui_kit::render::{Span, StyledLine};

use super::app::{Action, App, Overlay};
use super::cards::Card;
use super::page_frame::PageBody;
use super::pick::MEASURE_MAX;

/// 问入口的一个问题（Puffin 的 `UsageQuestion`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanAsk {
    Plans { provider: String },
    Quota { plan: String },
    Detail { plan: String, range: UsageRange },
}

/// 入口的回答（Puffin `UsageMsg`）。
#[derive(Debug, Clone, PartialEq)]
pub enum PlanAnswer {
    Plans {
        provider: String,
        result: Result<(UsageModel, Vec<UsagePlan>), String>,
    },
    Quota {
        plan: String,
        result: Result<PlanQuota, String>,
    },
    Detail {
        plan: String,
        range: UsageRange,
        /// 装箱：历史比别的回答大得多（同 Puffin `data::detail`）。
        result: Result<Box<PlanDetail>, String>,
    },
}

/// 这次运行读到的 `/usage`（Puffin `UsageUi`，没有 `at`：开着的是 `app.overlay` 的卡）。
#[derive(Debug, Clone, Default)]
pub struct UsageUi {
    pub provider_id: String,
    pub model: Option<UsageModel>,
    pub plans: Vec<UsagePlan>,
    pub plans_read: bool,
    pub plans_failed: Option<String>,
    pub quotas: BTreeMap<String, Seen<PlanQuota>>,
    pub details: BTreeMap<(String, String), Seen<PlanDetail>>,
    /// 在问的：`plans`、`quota:id`、`detail:id:range`。
    pub asking: BTreeSet<String>,
    /// 最近一个值到达的时刻（淡入时按动画节拍重画）。
    pub arrived: Option<Instant>,
    /// 失败回合（按视图行 id）的上限与在问的（usage_back.rs，Puffin `walls`、`walls_asking`）。
    pub walls: BTreeMap<u64, super::usage_back::Wall>,
    pub walls_asking: BTreeSet<u64>,
}

impl UsageUi {
    fn kit(&self) -> Usage<'_> {
        Usage {
            plans: &self.plans,
            plans_failed: self.plans_failed.as_deref(),
            quotas: &self.quotas,
            details: &self.details,
        }
    }

    /// 卡的条目：ZCode 不跑当前模型时它在最前，然后是计划。
    pub fn entries(&self) -> Vec<Entry> {
        self.model
            .iter()
            .filter(|m| !m.official && !m.provider_id.is_empty())
            .map(|m| Entry::Model(m.clone()))
            .chain(self.plans.iter().map(|p| Entry::Plan(p.clone())))
            .collect()
    }

    fn plan_at(&self, entry: usize) -> Option<UsagePlan> {
        match self.entries().into_iter().nth(entry) {
            Some(Entry::Plan(p)) => Some(p),
            _ => None,
        }
    }

    fn ask_plans(&mut self, asks: &mut Vec<PlanAsk>) {
        if self.asking.insert("plans".to_owned()) {
            asks.push(PlanAsk::Plans {
                provider: self.provider_id.clone(),
            });
        }
    }

    fn ask_quota(&mut self, p: &UsagePlan, asks: &mut Vec<PlanAsk>) {
        if self.asking.insert(format!("quota:{}", p.id)) {
            asks.push(PlanAsk::Quota { plan: p.id.clone() });
        }
    }

    fn ask_detail(&mut self, p: &UsagePlan, at: usize, asks: &mut Vec<PlanAsk>) {
        let r = range(at).0;
        if self
            .asking
            .insert(format!("detail:{}:{}", p.id, range_name(r)))
        {
            asks.push(PlanAsk::Detail {
                plan: p.id.clone(),
                range: r,
            });
        }
    }
}

fn asked(asks: Vec<PlanAsk>) -> Option<Action> {
    (!asks.is_empty()).then_some(Action::PlanUsage(asks))
}

fn keys(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    key_hints::pairs(pairs)
}

impl App {
    /// `/usage`：当前模型的计划的卡；每次打开都再问一次（Puffin `usage_open`）。草稿清空、输入层的卡换成它。
    pub fn open_plan_usage(&mut self) -> Option<Action> {
        self.composer.clear();
        let provider = self.view.selection().provider.clone();
        let ui = &mut self.usage_plans;
        // 换了模型时计划要重新问；额度与历史按计划 id 留着。
        if ui.provider_id != provider {
            ui.provider_id = provider;
            ui.model = None;
            ui.plans = Vec::new();
            ui.plans_read = false;
        }
        ui.plans_failed = None;
        self.overlay = Some(Overlay::Card(Card::Usage { entry: 0 }));
        let mut asks = Vec::new();
        ui.ask_plans(&mut asks);
        // 已有计划时卡片立即画出上一次的结果，同时再问一次额度。
        if let Some(p) = ui.plan_at(0) {
            ui.ask_quota(&p, &mut asks);
        }
        asked(asks)
    }

    /// 入口回答了（Puffin `usage_answered`）。到达按帧钟记；墙上时间（`_now_ms`）留着给调用处不变。
    pub fn plan_answered(&mut self, answer: PlanAnswer, _now_ms: u64) -> Option<Action> {
        // 缺陷修复（孪生 usage-page 第 35 步）：淡入的年龄以前按墙上时间的节拍算，Puffin 按动画的 tick（`state.clock`，只随帧
        // 走），没有帧的那段时间（等回答）也算进了年龄，淡得比 Puffin 快。到达与画都按状态栏的帧钟。
        let clock = self.status_motion.tick();
        let at = match &self.overlay {
            Some(Overlay::Card(card @ (Card::Usage { .. } | Card::UsagePage { .. }))) => {
                Some(card.clone())
            }
            _ => None,
        };
        let ui = &mut self.usage_plans;
        let mut asks = Vec::new();
        match answer {
            PlanAnswer::Plans { provider, result } => {
                ui.asking.remove("plans");
                if provider != ui.provider_id {
                    return None;
                }
                match result {
                    // 有上一次的计划时留着；没有时卡片说读不到。
                    Err(why) if !ui.plans_read => ui.plans_failed = Some(why),
                    Err(_) => {}
                    Ok((model, plans)) => {
                        ui.model = Some(model);
                        ui.plans = plans;
                        ui.plans_read = true;
                        ui.plans_failed = None;
                        let last = ui.entries().len().saturating_sub(1);
                        let clamped = at.map(|card| match card {
                            Card::Usage { entry } => Card::Usage {
                                entry: entry.min(last),
                            },
                            Card::UsagePage { entry, range, top } => Card::UsagePage {
                                entry: entry.min(last),
                                range,
                                top,
                            },
                            other => other,
                        });
                        if let Some(Card::Usage { entry }) = &clamped
                            && let Some(p) = ui.plan_at(*entry)
                        {
                            ui.ask_quota(&p, &mut asks);
                        }
                        if let Some(card) = clamped {
                            self.overlay = Some(Overlay::Card(card));
                        }
                    }
                }
            }
            PlanAnswer::Quota { plan, result } => {
                let seen = ui.quotas.remove(&plan).unwrap_or_default();
                let next = match result {
                    Ok(q) => seen.got(q, clock),
                    Err(why) => seen.missed(why),
                };
                ui.asking.remove(&format!("quota:{plan}"));
                ui.quotas.insert(plan, next);
                ui.arrived = Some(Instant::now());
            }
            PlanAnswer::Detail {
                plan,
                range,
                result,
            } => {
                let name = range_name(range).to_owned();
                let key = (plan.clone(), name.clone());
                let seen = ui.details.remove(&key).unwrap_or_default();
                let next = match result {
                    Ok(d) => seen.got(*d, clock),
                    Err(why) => seen.missed(why),
                };
                ui.asking.remove(&format!("detail:{plan}:{name}"));
                ui.details.insert(key, next);
                ui.arrived = Some(Instant::now());
            }
        }
        asked(asks)
    }

    /// 卡或页有值在淡入：按动画节拍重画（Puffin `usage_moving`）。
    pub fn usage_moving(&self) -> bool {
        let ticks = u128::from(TICK_MS) * u128::from(fade::TICKS.unsigned_abs());
        matches!(
            self.overlay,
            Some(Overlay::Card(Card::Usage { .. } | Card::UsagePage { .. }))
        ) && self
            .usage_plans
            .arrived
            .is_some_and(|at| at.elapsed().as_millis() < ticks)
    }

    /// 卡的行与槽（cards.rs 的 `card_layer` 放在板上；Puffin `usage_card_rows`、`usage_card_slot`）。
    pub(super) fn usage_card_layer(
        &self,
        entry: usize,
        width: usize,
        tick: i64,
        now_ms: u64,
    ) -> (Vec<StyledLine>, Vec<Span>) {
        let theme = &self.ink.theme;
        let ui = &self.usage_plans;
        let entries = ui.entries();
        let rows: Vec<Element> = if ui.plans_read && entries.is_empty() {
            let none = UsagePlan {
                id: String::new(),
                family: Family::Zai,
                kind: PlanKind::Individual,
                name: String::new(),
            };
            let q = PlanQuota {
                state: QuotaState::NoPlan,
                ..PlanQuota::default()
            };
            let mut out = vec![view::head(theme, Vec::new(), Vec::new()), br()];
            out.extend(view::said(view::missing(theme, &none, &q)));
            out
        } else {
            view::card(
                theme,
                &ui.kit(),
                entries.get(entry),
                &self.view.selection().model,
                now_ms as f64,
                &self.zone,
                tick,
            )
        };
        let lines = Layout::lay(&measure(stack(rows), MEASURE_MAX, 0), width.max(1));
        let slot = match entries.get(entry) {
            None if ui.plans_failed.is_some() => keys(&[("⏎", "try again")]),
            None => Vec::new(),
            Some(Entry::Model(_)) => ui
                .plans
                .first()
                .map(|p| vec![("←→".to_owned(), view::short_name(p))])
                .unwrap_or_default(),
            Some(Entry::Plan(p)) => {
                let seen = ui.quotas.get(&p.id).cloned().unwrap_or_default();
                let site = view::site(p.family);
                match (&seen.value, &seen.failed) {
                    (Some(_), _) if ui.asking.contains(&format!("quota:{}", p.id)) => {
                        return (
                            lines,
                            vec![Span::new(format!("Updating from {site}"), theme.fg3)],
                        );
                    }
                    (Some(q), Some(_)) => {
                        let at = view::clock(q.at as f64, &self.zone);
                        return (
                            lines,
                            vec![Span::new(
                                format!("Could not reach {site} · as of {at}"),
                                theme.fg3,
                            )],
                        );
                    }
                    (None, Some(_)) => keys(&[("⏎", "try again")]),
                    _ => {
                        let mut out = Vec::new();
                        if entries.len() > 1 {
                            out.extend(keys(&[("←→", "plan")]));
                        }
                        if view::detailed(p) {
                            out.extend(keys(&[("⏎", "details")]));
                        }
                        out
                    }
                }
            }
        };
        (lines, key_hints::spans(theme, &key_hints::pick(&slot)))
    }

    /// 卡的键（Puffin `usage_card_key`）：`None` 把键交下去（打字进草稿并关上卡）。
    pub(super) fn usage_card_key(&mut self, key: KeyEvent, entry: usize) -> Option<Option<Action>> {
        let entries = self.usage_plans.entries();
        let n = entries.len();
        let ctrl_o =
            key.code == KeyCode::Char('o') && key.modifiers.contains(KeyModifiers::CONTROL);
        let to = |app: &mut App, i: usize| {
            app.overlay = Some(Overlay::Card(Card::Usage { entry: i }));
            let ui = &mut app.usage_plans;
            let mut asks = Vec::new();
            if let Some(p) = ui.plan_at(i) {
                ui.ask_quota(&p, &mut asks);
            }
            asked(asks)
        };
        match key.code {
            KeyCode::Esc => {
                self.overlay = None;
                Some(None)
            }
            KeyCode::Left if n > 1 => Some(to(self, (entry + n - 1) % n)),
            KeyCode::Right if n > 1 => Some(to(self, (entry + 1) % n)),
            KeyCode::Left | KeyCode::Right => Some(None),
            KeyCode::Enter => Some(self.usage_enter(&entries, entry, true)?),
            _ if ctrl_o => self.usage_enter(&entries, entry, false),
            _ => None,
        }
    }

    /// 卡上的 ⏎（`enter`）或 ^O：再试一次、打开详情页；Start Plan 与不是官方的 provider 没有详情页，^O 照常展开细节。
    fn usage_enter(
        &mut self,
        entries: &[Entry],
        entry: usize,
        enter: bool,
    ) -> Option<Option<Action>> {
        let ui = &mut self.usage_plans;
        let mut asks = Vec::new();
        match entries.get(entry) {
            None if ui.plans_failed.is_some() && enter => {
                ui.plans_failed = None;
                ui.ask_plans(&mut asks);
                Some(asked(asks))
            }
            Some(Entry::Plan(p))
                if enter
                    && ui
                        .quotas
                        .get(&p.id)
                        .is_some_and(|s| s.value.is_none() && s.failed.is_some()) =>
            {
                ui.ask_quota(p, &mut asks);
                Some(asked(asks))
            }
            Some(Entry::Plan(p)) if view::detailed(p) => {
                Some(self.usage_open_page(entry, DEFAULT_RANGE))
            }
            _ if enter => Some(None),
            _ => None,
        }
    }

    pub(super) fn usage_open_page(&mut self, entry: usize, at: usize) -> Option<Action> {
        self.overlay = Some(Overlay::Card(Card::UsagePage {
            entry,
            range: at,
            top: 0,
        }));
        let ui = &mut self.usage_plans;
        let p = ui.plan_at(entry)?;
        let mut asks = Vec::new();
        if ui.quotas.get(&p.id).is_none_or(|s| s.value.is_none()) {
            ui.ask_quota(&p, &mut asks);
        }
        ui.ask_detail(&p, at, &mut asks);
        asked(asks)
    }

    /// 页的键（Puffin `usage_page_key`）：滚动按上一帧页的几何（`page_rows`：正文的行数与页的行数）。
    pub(super) fn usage_page_key(
        &mut self,
        key: KeyEvent,
        (entry, range, top): (usize, usize, usize),
    ) -> Option<Option<Action>> {
        let (rows, total) = self.page_rows;
        let max_top = total.saturating_sub(rows);
        let step = rows.saturating_sub(1).max(1) as i64;
        let by = match key.code {
            KeyCode::Up => Some(-1),
            KeyCode::Down => Some(1),
            KeyCode::PageUp => Some(-step),
            KeyCode::PageDown => Some(step),
            _ => None,
        };
        if let Some(by) = by {
            let top = (top.min(max_top) as i64 + by).clamp(0, max_top as i64) as usize;
            self.overlay = Some(Overlay::Card(Card::UsagePage { entry, range, top }));
            return Some(None);
        }
        let ranges = RANGES.len();
        match key.code {
            KeyCode::Esc => {
                self.overlay = Some(Overlay::Card(Card::Usage { entry }));
                Some(None)
            }
            KeyCode::Left => Some(self.usage_open_page(entry, (range + ranges - 1) % ranges)),
            KeyCode::Right => Some(self.usage_open_page(entry, (range + 1) % ranges)),
            KeyCode::Tab => {
                let pages: Vec<usize> = self
                    .usage_plans
                    .entries()
                    .iter()
                    .enumerate()
                    .filter_map(|(i, e)| match e {
                        Entry::Plan(p) if view::detailed(p) => Some(i),
                        _ => None,
                    })
                    .collect();
                // 最后一个计划之后是「This computer」（usage_local.rs，Puffin 没有；它的槽不变，tui-kit.md §4.3）。
                Some(match pages.iter().copied().find(|i| *i > entry) {
                    Some(next) => self.usage_open_page(next, range),
                    None => Some(super::usage_local::open_action()),
                })
            }
            KeyCode::Enter => Some(None),
            _ => None,
        }
    }

    /// 开着的详情页（`page_frame::body`；Puffin `usage_frame`）：有请求在等时不画（Puffin `usage_page_of`）。
    /// `tick` 是这一帧的动画节拍（淡入的年龄从回答到达的那一帧起算）。
    pub fn usage_page_body(&self, width: usize, tick: i64, now_ms: u64) -> Option<PageBody> {
        let Some(Overlay::Card(Card::UsagePage { entry, range, top })) = self.overlay else {
            return None;
        };
        if self.asking() {
            return None;
        }
        let ui = &self.usage_plans;
        let rows = ui.plan_at(entry).map_or_else(Vec::new, |plan| {
            page::page(
                &self.ink.theme,
                &ui.kit(),
                &plan,
                range,
                now_ms as f64,
                &self.zone,
                tick,
            )
        });
        let lines = Layout::lay(&measure(stack(rows), MEASURE_MAX, 0), width.max(1));
        let mut slot = Vec::new();
        if ui.plans.iter().filter(|p| view::detailed(p)).count() > 1 {
            slot.extend(keys(&[("⇥", "plan")]));
        }
        slot.extend(keys(&[("←→", "range")]));
        Some(PageBody {
            lines,
            top,
            slot: Some(key_hints::spans(&self.ink.theme, &key_hints::pick(&slot))),
            owns_keys: false,
            fit: false,
        })
    }
}

#[cfg(test)]
#[path = "usage_plans_tests.rs"]
mod tests;
