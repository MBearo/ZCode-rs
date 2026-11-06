//! `/usage` 详情页多出的一个标签页「This computer」（docs/specs/rust-cli/usage.md §4、§5；tui-kit.md §4.3 登记）：这次
//! 会话的 API 时长与墙钟时长、改动的行与各模型的 token，和这台电脑的活动统计（消息的热力图、范围内的 token、会话与模型）。
//! Puffin 的详情页只有计划；这一页是它的样子（kit `usage::page` 的面包屑、段标题、`totals` 的行、热力图），只在计划的页
//! 之后才到得了：最后一个计划的页按 ⇥，或 `/usage all` 直接打开。
//!
//! 数据是内核的（用量表是唯一来源）：打开时 runner 一起问 API 时长与全时段的统计，换范围时再问一次，回执带页的请求序号，
//! 过时的丢掉（`FuturesUnordered` 里回执的先后不定），不用超时。行只用 kit 的构件，句子是英文（同 Puffin 的卡与页）。
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::metering::{
    ActivityRange, ActivityStats, SessionMetering, SessionSummary,
};
use rust_cli_tui_kit::app::element::{Element, br, by_width, measure, single_line, stack};
use rust_cli_tui_kit::app::layout::Layout;
use rust_cli_tui_kit::app::widgets::key_hints;
use rust_cli_tui_kit::app::widgets::stats::working;
use rust_cli_tui_kit::app::widgets::usage::view::{
    LEAD, SETTLED, asking, commas, compact, detailed, facts, failed, joined, pad_to, short_name,
    spread,
};
use rust_cli_tui_kit::app::widgets::usage::{PlanDetail, page};
use rust_cli_tui_kit::render::{Span, Style, attr};
use rust_cli_tui_kit::theme::theme::Theme;

use super::app::{Action, App, Overlay};
use super::manage::Manage;
use super::page_frame::PageBody;
use super::pick::MEASURE_MAX;
use super::usage_local_data as data;

/// 这一页的名字（面包屑与右端的标签）。
pub const NAME: &str = "This computer";

/// 范围与它们的标签（照 kit `RANGES` 的写法）。
const RANGES: [(ActivityRange, &str); 3] = [
    (ActivityRange::All, "All time"),
    (ActivityRange::Week, "7 days"),
    (ActivityRange::Month, "30 days"),
];

/// 打开的这一页：会话的数字（打开时算好）、全时段的统计、所选的范围与它的统计（`None` 时还在问）、请求序号与滚动。
#[derive(Debug, Clone, PartialEq)]
pub struct LocalView {
    pub session: SessionSummary,
    /// `Err` 是给用户看的原因。
    pub all: Result<ActivityStats, String>,
    pub range: ActivityRange,
    pub ranged: Option<Result<ActivityStats, String>>,
    pub seq: u64,
    pub top: usize,
}

impl LocalView {
    /// 滚轮与 ↑↓：夹到最后一屏（`max_top`：上一帧页的行数减正文的行数，`App::page_rows`）。
    pub fn wheel(&mut self, lines: isize, max_top: usize) {
        self.top = self
            .top
            .min(max_top)
            .saturating_add_signed(lines)
            .min(max_top);
    }

    /// 所选范围的统计：全时段用打开时的；其余在回执到之前为 `None`。
    fn shown(&self) -> Option<&Result<ActivityStats, String>> {
        match self.range {
            ActivityRange::All => Some(&self.all),
            _ => self.ranged.as_ref(),
        }
    }
}

impl App {
    /// 会话的数字：墙钟时长从本次 TUI 启动起算，改动来自 `FileChanges`，用量按模型累计。
    pub(crate) fn session_summary(&self, metering: SessionMetering) -> SessionSummary {
        let wall_duration_ms =
            u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let (lines_added, lines_removed) = self
            .view
            .turn_changes()
            .iter()
            .flat_map(|turn| &turn.files)
            .fold((0, 0), |(a, d), f| (a + f.additions, d + f.deletions));
        SessionSummary {
            api_duration_ms: metering.api_duration_ms,
            wall_duration_ms,
            lines_added,
            lines_removed,
            models: self
                .view
                .usage()
                .iter()
                .map(|(model, usage)| (model.clone(), *usage))
                .collect(),
        }
    }

    /// 打开这一页：API 时长与全时段的统计到了。草稿清空（同 Puffin 打开卡）。
    pub fn open_usage(&mut self, metering: SessionMetering, all: Result<ActivityStats, String>) {
        self.composer.clear();
        self.overlay = Some(Overlay::Usage(Box::new(LocalView {
            session: self.session_summary(metering),
            all,
            range: ActivityRange::All,
            ranged: None,
            seq: 0,
            top: 0,
        })));
    }

    /// 范围的回执：页还开着、序号是最新的才收下。
    pub fn usage_range(&mut self, seq: u64, result: Result<ActivityStats, String>) {
        if let Some(Overlay::Usage(view)) = self.overlay.as_mut()
            && view.seq == seq
        {
            view.ranged = Some(result);
        }
    }

    /// 页的键（同计划的页）：←→ 换范围，⇥ 到第一个计划的页，esc 回到卡，↑↓ 与翻页滚动。
    pub(super) fn usage_key(&mut self, key: KeyEvent) -> Option<Action> {
        let (rows, total) = self.page_rows;
        let max_top = total.saturating_sub(rows);
        let step = rows.saturating_sub(1).max(1) as isize;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let first_page = self.usage_plans.entries().iter().position(
            |e| matches!(e, rust_cli_tui_kit::app::widgets::usage::Entry::Plan(p) if detailed(p)),
        );
        let Some(Overlay::Usage(view)) = self.overlay.as_mut() else {
            return None;
        };
        let by = match key.code {
            KeyCode::Up => Some(-1),
            KeyCode::Down => Some(1),
            KeyCode::PageUp => Some(-step),
            KeyCode::PageDown => Some(step),
            _ => None,
        };
        if let Some(by) = by {
            view.wheel(by, max_top);
            return None;
        }
        let at = RANGES
            .iter()
            .position(|(r, _)| *r == view.range)
            .unwrap_or(0);
        let to = match key.code {
            KeyCode::Left => Some(RANGES[(at + RANGES.len() - 1) % RANGES.len()].0),
            KeyCode::Right => Some(RANGES[(at + 1) % RANGES.len()].0),
            _ => None,
        };
        if let Some(range) = to {
            view.range = range;
            view.seq += 1;
            view.ranged = None;
            view.top = 0;
            return (range != ActivityRange::All).then_some(Action::Manage(Manage::UsageRange {
                range,
                seq: view.seq,
            }));
        }
        match key.code {
            KeyCode::Esc => self.open_plan_usage(),
            KeyCode::Char('c') if ctrl => {
                self.overlay = None;
                None
            }
            KeyCode::Tab => {
                let entry = first_page?;
                self.usage_open_page(entry, rust_cli_tui_kit::app::widgets::usage::DEFAULT_RANGE)
            }
            _ => None,
        }
    }
}

fn heading(theme: &Theme, text: &str) -> Vec<Span> {
    vec![
        Span::plain(LEAD),
        Span::new(text.to_owned(), theme.fg2.with_attr(attr::BOLD)),
    ]
}

fn headed(theme: &Theme, text: &str, right: Vec<Span>) -> Element {
    let left = heading(theme, text);
    by_width(move |w| single_line(spread(left.clone(), right.clone(), w, 2), false))
}

/// `totals` 的一行：第 3 列起 fg3 的名字补到 12 格，然后是值；值是空的时不写这一行。
fn row(theme: &Theme, name: &str, spans: Vec<Span>) -> Option<Element> {
    (!spans.is_empty()).then(|| {
        let mut all = vec![Span::plain(LEAD), Span::new(pad_to(name, 12), theme.fg3)];
        all.extend(spans);
        single_line(all, false)
    })
}

fn chosen(theme: &Theme) -> Style {
    if theme.rich {
        Style::fg(theme.brand)
    } else {
        Style::fg(theme.brand).with_attr(attr::BOLD)
    }
}

/// 页的行，从上到下：面包屑、这次会话、消息的热力图、范围内的合计。
pub fn rows(theme: &Theme, view: &LocalView, app: &App) -> Vec<Element> {
    let theme = *theme;
    // 面包屑：有计划的页时右端是各页（计划与这一页），这一页是选中的墨。
    let pages: Vec<String> = app
        .usage_plans
        .plans
        .iter()
        .filter(|p| detailed(p))
        .map(short_name)
        .collect();
    let mut tabs = Vec::new();
    if !pages.is_empty() {
        for name in pages {
            tabs.push(Span::new(name, theme.fg3));
            tabs.push(Span::plain("   "));
        }
        tabs.push(Span::new(NAME, chosen(&theme)));
    }
    let left = vec![
        Span::plain(LEAD),
        Span::new("Usage", Style::DEFAULT.with_attr(attr::BOLD)),
        Span::new("  ›  ", theme.line),
        Span::plain(NAME),
    ];
    let crumb = by_width(move |w| single_line(spread(left.clone(), tabs.clone(), w, 3), false));
    let s = &view.session;
    let mut out = vec![
        br(),
        crumb,
        br(),
        headed(
            &theme,
            "This session",
            facts(
                &theme,
                vec![
                    (working(s.api_duration_ms as f64), "API time".to_owned()),
                    (working(s.wall_duration_ms as f64), "wall time".to_owned()),
                ],
            ),
        ),
    ];
    let changes = if s.lines_added + s.lines_removed == 0 {
        Vec::new()
    } else {
        vec![
            Span::new(
                format!("+{}", commas(s.lines_added as f64)),
                Style::fg(theme.ok),
            ),
            Span::plain(" "),
            Span::new(
                format!("−{}", commas(s.lines_removed as f64)),
                Style::fg(theme.err),
            ),
            Span::new(" lines", theme.fg3),
        ]
    };
    let models = s
        .models
        .iter()
        .map(|(name, u)| {
            let tokens = u.input_tokens
                + u.output_tokens
                + u.cache_creation_input_tokens
                + u.cache_read_input_tokens;
            (name.clone(), compact(tokens as f64))
        })
        .collect();
    out.extend(row(&theme, "Changes", changes));
    out.extend(row(&theme, "Models", joined(&theme, models)));
    out.push(br());
    // 消息的热力图：总是全时段的（同 Puffin 的「Tokens, last 52 weeks」不随范围）。
    match &view.all {
        Err(why) => out.push(failed(
            &theme,
            &format!("Could not read this computer's history: {why}"),
        )),
        Ok(all) => {
            let detail = PlanDetail {
                activity: data::activity(all),
                ..PlanDetail::default()
            };
            let a = &detail.activity;
            let mut pairs = Vec::new();
            if a.total > 0 {
                pairs.push((compact(a.total as f64), "total".to_owned()));
            }
            if a.best_day > 0 {
                pairs.push((compact(a.best_day as f64), "best day".to_owned()));
            }
            if a.streak > 0 {
                pairs.push((format!("{}-day", a.streak), "streak".to_owned()));
            }
            out.push(headed(
                &theme,
                "Messages, last 52 weeks",
                facts(&theme, pairs),
            ));
            if a.levels.iter().flatten().all(|l| *l <= 0) {
                out.push(single_line(
                    vec![
                        Span::plain(LEAD),
                        Span::new("No activity yet.", theme.fg3.with_attr(attr::ITALIC)),
                    ],
                    false,
                ));
            } else {
                out.push(by_width(move |w| {
                    stack(page::heat_rows(&theme, &detail, w, SETTLED))
                }));
            }
        }
    }
    out.push(br());
    // 范围内的合计：段标题后面是各范围（同 Puffin「Tokens by day」后面的范围）。
    let mut head = heading(&theme, "On this computer");
    for (i, (range, label)) in RANGES.iter().enumerate() {
        head.push(Span::plain(if i == 0 { "   " } else { "  " }));
        head.push(Span::new(
            *label,
            if *range == view.range {
                chosen(&theme)
            } else {
                theme.fg3
            },
        ));
    }
    out.push(single_line(head, false));
    match view.shown() {
        None => out.push(asking(&theme, "Reading this computer's history")),
        Some(Err(why)) => out.push(failed(
            &theme,
            &format!("Could not read the history: {why}"),
        )),
        Some(Ok(stats)) => {
            let tokens: u64 = stats
                .models
                .iter()
                .map(rust_cli_kernel_api::metering::ModelTokens::total)
                .sum();
            let (_, longest) = data::streaks(stats);
            let mut total = Vec::new();
            if tokens > 0 {
                total.push((compact(tokens as f64), "tokens".to_owned()));
            }
            if stats.sessions > 0 {
                total.push((
                    commas(stats.sessions as f64),
                    plural(stats.sessions, "session"),
                ));
            }
            if !stats.days.is_empty() {
                let n = stats.days.len() as u64;
                total.push((commas(n as f64), plural(n, "active day")));
            }
            let models = stats
                .models
                .iter()
                .map(|m| (m.model.clone(), compact(m.total() as f64)))
                .collect();
            let mut longest_facts = Vec::new();
            if let Some(ms) = stats.longest_session_ms {
                longest_facts.push((working(ms as f64), "session".to_owned()));
            }
            if longest > 0 {
                longest_facts.push((format!("{longest}-day"), "streak".to_owned()));
            }
            let rows: Vec<Element> = [
                row(&theme, "Total", facts(&theme, total)),
                row(&theme, "Models", joined(&theme, models)),
                row(&theme, "Longest", facts(&theme, longest_facts)),
            ]
            .into_iter()
            .flatten()
            .collect();
            if rows.is_empty() {
                out.push(single_line(
                    vec![
                        Span::plain(LEAD),
                        Span::new("No activity yet.", theme.fg3.with_attr(attr::ITALIC)),
                    ],
                    false,
                ));
            }
            out.extend(rows);
        }
    }
    out
}

fn plural(n: u64, word: &str) -> String {
    if n == 1 {
        word.to_owned()
    } else {
        format!("{word}s")
    }
}

/// 开着的这一页（`page_frame::body`）：代替正文，键在槽里（有计划的页时 `⇥ plan`，然后 `←→ range`）。
pub fn page_body(app: &App, width: usize) -> Option<PageBody> {
    let Some(Overlay::Usage(view)) = &app.overlay else {
        return None;
    };
    let theme = &app.ink.theme;
    let lines = Layout::lay(
        &measure(stack(rows(theme, view, app)), MEASURE_MAX, 0),
        width.max(1),
    );
    let mut slot = Vec::new();
    if app.usage_plans.plans.iter().any(detailed) {
        slot.extend(key_hints::pairs(&[("⇥", "plan")]));
    }
    slot.extend(key_hints::pairs(&[("←→", "range")]));
    Some(PageBody {
        lines,
        top: view.top,
        slot: Some(key_hints::spans(theme, &key_hints::pick(&slot))),
        owns_keys: true,
        fit: false,
    })
}

/// 计划的页按 ⇥ 走过最后一个计划时到这一页（`usage_plans.rs`）。
pub fn open_action() -> Action {
    Action::Manage(Manage::Usage)
}

#[cfg(test)]
#[path = "usage_local_tests.rs"]
mod tests;
