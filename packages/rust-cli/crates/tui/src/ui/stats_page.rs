//! 统计的页（docs/specs/rust-cli/tui-panels.md §4.1）：`/stats` 卡上 ⏎ 或 ⇥ 打开，代替正文（`page_frame.rs`）。两个标签：
//! Context（`context_count.rs`、`context_page.rs`）与 Session（`session_page.rs`）。标签的名字在面包屑的右端（同 `/usage` 的
//! 详情页），⇥ 换标签，esc 回到卡；每个标签的状态在页开着时留着。`/status` 打开卡并直接打开 Session。
//!
//! 查看子代理时 Context 不能看（上下文只为主会话计数，§4.1）：它的名字是 line 色，⇥ 不去它，开着 Context 时按 Session 画。
use crossterm::event::{KeyCode, KeyEvent};
use rust_cli_tui_kit::app::widgets::fit::{MEASURE_MARGIN, MEASURE_MAX};
use rust_cli_tui_kit::app::widgets::lists::chosen_style;
use rust_cli_tui_kit::app::widgets::usage::view::{LEAD, spread};
use rust_cli_tui_kit::render::{Span, Style, StyledLine, attr};
use rust_cli_tui_kit::theme::theme::Theme;

use super::app::{Action, App, Overlay};
use super::cards::Card;
use super::context_count::ContextPage;
use super::page_frame::PageBody;

/// 页的标签。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Context,
    Session,
}

impl Tab {
    fn name(self) -> &'static str {
        match self {
            Self::Context => "Context",
            Self::Session => "Session",
        }
    }
}

/// 开着的页：显示的标签与两个标签各自的状态（Context 的开合、选中、顶行；Session 的顶行）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatsPage {
    pub tab: Tab,
    pub context: ContextPage,
    pub session_top: usize,
}

/// 标签名之间的空格（同 `/usage` 详情页）。
const TAB_GAP: &str = "   ";
/// 面包屑与右端的标签之间至少空这么多列，不够时不写标签。
const CRUMB_GAP: i64 = 3;

/// 面包屑 `Stats  ›  <标签>`，右端两个标签的名字：显示的 `chosen_style`，其余 fg3，不能看的 line 色。
pub fn crumb(theme: &Theme, width: usize, shown: Tab, context_off: bool) -> StyledLine {
    let cols = width.clamp(1, MEASURE_MAX + MEASURE_MARGIN);
    let left = vec![
        Span::plain(LEAD),
        Span::new("Stats", Style::DEFAULT.with_attr(attr::BOLD)),
        Span::new("  ›  ", theme.line),
        Span::plain(shown.name()),
    ];
    let mut right = Vec::new();
    for tab in [Tab::Context, Tab::Session] {
        if !right.is_empty() {
            right.push(Span::plain(TAB_GAP));
        }
        let style = if tab == shown {
            chosen_style(theme)
        } else if tab == Tab::Context && context_off {
            theme.line
        } else {
            theme.fg3
        };
        right.push(Span::new(tab.name(), style));
    }
    StyledLine::new(spread(left, right, cols, CRUMB_GAP))
}

impl App {
    /// 开着的页（不开着时 None）。
    pub(super) fn stats_page(&self) -> Option<&StatsPage> {
        match &self.overlay {
            Some(Overlay::Card(Card::StatsPage(page))) => Some(page),
            _ => None,
        }
    }

    /// 显示的标签：查看子代理时总是 Session（§4.1）。
    pub(super) fn stats_tab(&self) -> Option<Tab> {
        let page = self.stats_page()?;
        Some(if self.viewing.is_some() {
            Tab::Session
        } else {
            page.tab
        })
    }

    /// 打开页的一个标签（卡上 ⏎ / ⇥、`/stats all`、`/status`）；查看子代理时 Context 不开。Session 要问它的事实
    /// （session_page.rs）。
    pub(super) fn open_stats_page(&mut self, tab: Tab) -> Option<Action> {
        if tab == Tab::Context && self.viewing.is_some() {
            return None;
        }
        self.overlay = Some(Overlay::Card(Card::StatsPage(StatsPage {
            tab,
            ..StatsPage::default()
        })));
        (tab == Tab::Session).then_some(Action::SessionFacts)
    }

    /// 页上 ⇥：换到另一个标签，各自的状态留着。
    fn switch_stats_tab(&mut self) -> Option<Action> {
        let viewing = self.viewing.is_some();
        let Some(Overlay::Card(Card::StatsPage(page))) = self.overlay.as_mut() else {
            return None;
        };
        let next = match page.tab {
            Tab::Context => Tab::Session,
            Tab::Session if viewing => return None,
            Tab::Session => Tab::Context,
        };
        page.tab = next;
        (next == Tab::Session).then_some(Action::SessionFacts)
    }

    /// 开着的页（`page_frame::body`）：有请求在等时不画（同用量的详情页）。
    pub fn stats_page_body(&self, width: usize, now_ms: u64) -> Option<PageBody> {
        let page = self.stats_page()?;
        if self.asking() {
            return None;
        }
        Some(match self.stats_tab()? {
            Tab::Context => self.context_page_body(&page.context, width, now_ms),
            Tab::Session => self.session_page_body(page.session_top, width, now_ms),
        })
    }

    /// 页的键：⇥ 换标签，esc 回到卡（卡打开时已经问过计数，不再问），其余的给显示的标签；标签不要的键去草稿（打了字卡与页
    /// 一起关上，`settle_card`）。
    pub(super) fn stats_page_key(&mut self, key: KeyEvent) -> Option<Option<Action>> {
        let tab = self.stats_tab()?;
        match key.code {
            KeyCode::Esc => {
                let _ = self.open_stats();
                Some(None)
            }
            KeyCode::Tab => Some(self.switch_stats_tab()),
            _ => match tab {
                Tab::Context => self.context_page_key(key),
                Tab::Session => self.session_page_key(key),
            },
        }
    }

    /// 滚轮滚动显示的标签（Context 不动选中）。
    pub(super) fn stats_page_wheel(&mut self, lines: isize) -> bool {
        let Some(tab) = self.stats_tab() else {
            return false;
        };
        let (rows, total) = self.page_rows;
        let max_top = total.saturating_sub(rows);
        let Some(Overlay::Card(Card::StatsPage(page))) = self.overlay.as_mut() else {
            return false;
        };
        let top = match tab {
            Tab::Context => &mut page.context.top,
            Tab::Session => &mut page.session_top,
        };
        *top = (*top)
            .min(max_top)
            .saturating_add_signed(lines)
            .min(max_top);
        true
    }
}
