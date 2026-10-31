//! `/usage`（别名 `/cost`、`/stats`）的页面（docs/specs/rust-cli/usage.md §4、§5）：Usage 与 Stats 两个标签页。
//! Usage 是会话部分（API 时长由 runner 从内核查，用量表是唯一来源）；Stats 是活动统计，数据是内核
//! 给出的聚合。换日期范围时按请求序号丢掉过时的回执（`FuturesUnordered` 里回执的先后不定），不用超时。
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use rust_cli_kernel_api::metering::{
    ActivityRange, ActivityStats, SessionMetering, SessionSummary,
};

use super::app::{Action, App, Overlay};
use super::i18n_usage::{self as text, UsageStrings};
use super::manage::Manage;
use super::palette::Palette;
use super::usage_stats::{self as stats, MODELS_PER_PAGE};

/// Stats 的 Models 视图每次（一格滚轮、一次 ↑ / ↓）滚两个模型。
const MODELS_STEP: usize = 2;

/// 标签页（Usage 与 Stats，usage.md D11）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageTab {
    Usage,
    Stats,
}

/// Stats 标签页的两个视图。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatsView {
    Overview,
    Models,
}

/// 页面的视图状态；统计数据是内核查询的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct UsageView {
    pub tab: UsageTab,
    pub view: StatsView,
    /// Usage 标签页的正文（打开时算好）。
    pub summary: String,
    /// 全时段的统计；`Err` 是给用户看的错误。
    pub all: Result<ActivityStats, String>,
    pub range: ActivityRange,
    /// 所选范围（不是 All）的统计；None 时还在查。
    pub ranged: Option<Result<ActivityStats, String>>,
    /// 最新一次范围查询的序号。
    pub seq: u64,
    pub models_offset: usize,
    pub scroll: usize,
}

impl UsageView {
    /// 滚轮（tui-select.md §4.1）：与 ↑ / ↓ 相同，Stats 的 Models 每格两个模型，其余滚动 `lines` 行。
    pub fn wheel(&mut self, lines: isize) {
        if self.tab == UsageTab::Stats && self.view == StatsView::Models {
            let count = self.shown().0.as_ref().map_or(0, |s| s.models.len());
            let step = if lines > 0 {
                MODELS_STEP as isize
            } else {
                -(MODELS_STEP as isize)
            };
            self.models_offset = (self.models_offset as isize + step)
                .clamp(0, count.saturating_sub(MODELS_PER_PAGE) as isize)
                as usize;
        } else {
            self.scroll = self.scroll.saturating_add_signed(lines);
        }
    }

    /// 所选范围的统计：All 用全时段的结果；其余在查到之前显示上一份（标出加载中）。
    fn shown(&self) -> (&Result<ActivityStats, String>, bool) {
        match (&self.range, &self.ranged) {
            (ActivityRange::All, _) => (&self.all, false),
            (_, Some(ranged)) => (ranged, false),
            (_, None) => (&self.all, true),
        }
    }
}

impl App {
    /// 会话部分的输入：墙钟时长从本次 TUI 启动起算，代码改动来自 `FileChanges`，用量按模型累计。
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

    /// 打开页面：API 时长与全时段的统计已查到；`/stats` 开 Stats 标签页。
    pub fn open_usage(
        &mut self,
        metering: SessionMetering,
        all: Result<ActivityStats, String>,
        stats_tab: bool,
    ) {
        self.overlay = Some(Overlay::Usage(Box::new(UsageView {
            tab: if stats_tab {
                UsageTab::Stats
            } else {
                UsageTab::Usage
            },
            view: StatsView::Overview,
            summary: self.session_summary(metering).text(),
            all,
            range: ActivityRange::All,
            ranged: None,
            seq: 0,
            models_offset: 0,
            scroll: 0,
        })));
    }

    /// 范围查询的回执：页面还开着、序号是最新的才用。
    pub fn usage_range(&mut self, seq: u64, result: Result<ActivityStats, String>) {
        if let Some(Overlay::Usage(view)) = self.overlay.as_mut()
            && view.seq == seq
        {
            view.ranged = Some(result);
        }
    }

    pub(super) fn usage_key(&mut self, key: KeyEvent) -> Option<Action> {
        let Some(Overlay::Usage(view)) = self.overlay.as_mut() else {
            return None;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.overlay = None,
            KeyCode::Char('c') if ctrl => self.overlay = None,
            KeyCode::Tab | KeyCode::BackTab => {
                view.tab = match view.tab {
                    UsageTab::Usage => UsageTab::Stats,
                    UsageTab::Stats => UsageTab::Usage,
                };
                view.scroll = 0;
            }
            KeyCode::Left | KeyCode::Right if view.tab == UsageTab::Stats => {
                view.view = match view.view {
                    StatsView::Overview => StatsView::Models,
                    StatsView::Models => StatsView::Overview,
                };
                view.scroll = 0;
            }
            // `r`：All time → Last 7 days → Last 30 days；All 用已有的全时段结果，其余问内核。
            KeyCode::Char('r') if view.tab == UsageTab::Stats => {
                view.range = view.range.next();
                view.seq += 1;
                view.ranged = None;
                view.models_offset = 0;
                if view.range != ActivityRange::All {
                    return Some(Action::Manage(Manage::UsageRange {
                        range: view.range,
                        seq: view.seq,
                    }));
                }
            }
            // Models：每次滚两个；其余视图滚动整页。
            KeyCode::Down | KeyCode::Char('j') => {
                if view.tab == UsageTab::Stats && view.view == StatsView::Models {
                    let count = view.shown().0.as_ref().map_or(0, |s| s.models.len());
                    view.models_offset = (view.models_offset + MODELS_STEP)
                        .min(count.saturating_sub(MODELS_PER_PAGE));
                } else {
                    view.scroll += 1;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if view.tab == UsageTab::Stats && view.view == StatsView::Models {
                    view.models_offset = view.models_offset.saturating_sub(MODELS_STEP);
                } else {
                    view.scroll = view.scroll.saturating_sub(1);
                }
            }
            _ => {}
        }
        None
    }
}

/// 标签栏：选中的粗体强调色，其余 dim。
fn tabs<T: PartialEq>(
    items: &[(T, &'static str)],
    selected: &T,
    palette: &Palette,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (index, (item, label)) in items.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("   "));
        }
        spans.push(if item == selected {
            Span::styled(
                *label,
                Style::default()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(*label, palette.dim())
        });
    }
    spans
}

/// Usage 标签页：`Session` 标题、dim 的会话部分、dim 的 `Esc to cancel`。
fn usage_body(view: &UsageView, strings: &UsageStrings, palette: &Palette) -> Vec<Line<'static>> {
    let mut out = vec![
        Line::from(Span::styled(
            strings.session,
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::default(),
    ];
    out.extend(
        view.summary
            .lines()
            .map(|line| Line::from(Span::styled(line.to_owned(), palette.dim()))),
    );
    out.push(Line::default());
    out.push(Line::from(super::key_hint::spans(
        strings.esc_cancel,
        palette,
    )));
    out
}

fn stats_body(view: &UsageView, width: usize, app: &App) -> Vec<Line<'static>> {
    let strings = text::strings(app.lang);
    let palette = &app.palette;
    let mut out = vec![
        Line::from(tabs(
            &[
                (StatsView::Overview, strings.overview),
                (StatsView::Models, strings.models),
            ],
            &view.view,
            palette,
        )),
        Line::default(),
    ];
    let all = match &view.all {
        Err(error) => {
            out.push(Line::from(Span::styled(error.clone(), palette.dim())));
            return out;
        }
        // 一个会话都没有时只给提示。
        Ok(all) if all.sessions == 0 => {
            out.push(stats::empty(app.lang));
            return out;
        }
        Ok(all) => all,
    };
    let (shown, loading) = view.shown();
    let shown = match shown {
        Ok(shown) => shown,
        Err(error) => {
            out.push(Line::from(Span::styled(error.clone(), palette.dim())));
            return out;
        }
    };
    out.extend(match view.view {
        StatsView::Overview => {
            stats::overview(shown, all, view.range, loading, width, palette, app.lang)
        }
        StatsView::Models => stats::models(
            shown,
            view.range,
            loading,
            view.models_offset,
            palette,
            app.lang,
        ),
    });
    out
}

/// 整屏页面：第一行是标签栏，空一行，正文（左缩进两格），最后一行 dim 的提示。
pub fn draw(frame: &mut Frame<'_>, app: &mut App, view: &UsageView) {
    let area = frame.area();
    let strings = text::strings(app.lang);
    let width = usize::from(area.width).saturating_sub(super::layout::INDENT);
    let height = super::page::body_height(area.height);
    app.viewport = height;
    let mut header = vec![Span::raw("  ")];
    header.extend(tabs(
        &[
            (UsageTab::Usage, strings.usage_tab),
            (UsageTab::Stats, strings.stats_tab),
        ],
        &view.tab,
        &app.palette,
    ));
    let body = match view.tab {
        UsageTab::Usage => usage_body(view, strings, &app.palette),
        UsageTab::Stats => stats_body(view, width, app),
    };
    let scroll = view.scroll.min(body.len().saturating_sub(height));
    // 夹住之后写回（F25：原来不写回，往回滚要多滚很多格）。
    if let Some(Overlay::Usage(stored)) = app.overlay.as_mut() {
        stored.scroll = scroll;
    }
    let mut out = vec![Line::from(header), Line::default()];
    out.extend(body.into_iter().skip(scroll).take(height).map(|line| {
        let mut spans = vec![Span::raw("  ")];
        spans.extend(line.spans);
        Line::from(spans)
    }));
    frame.render_widget(Paragraph::new(out), area);
    let hint = match view.tab {
        UsageTab::Usage => strings.usage_hint,
        UsageTab::Stats => strings.stats_hint,
    };
    let mut spans = vec![Span::raw("  ")];
    spans.extend(super::footer::hint(hint, &app.palette));
    frame.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1),
    );
}

#[cfg(test)]
#[path = "usage_tests.rs"]
mod tests;
