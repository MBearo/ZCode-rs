//! 上下文的计数与它的页（docs/specs/rust-cli/tui-panels.md §4.1、§4.3）：一次 `Kernel::context_usage` 的答案是
//! `/stats` 卡的组成与上下文的页唯一的数据源（[`ContextCount`]）。卡或页开着时 runner 问（`runner/services.rs` 的
//! `count_context`）；答案带着问的会话，回来时看的已经不是它就丢掉、再问一次。页的状态（开合、选中、顶行）在统计的页的
//! Context 标签里（`stats_page.rs`），按路径记。
//!
//! 只问主会话：内核的 `context_usage` 按 ID 打开会话（`open_session` 冷加载），子代理的子会话由 agents 驱动，
//! 再开一个 actor 就有两个 owner。查看子代理时卡没有组成，也没有 `⏎ context`，页上 Context 不能看（§4.1）。
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use rust_cli_kernel_api::context_usage::ContextUsage;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_tui_kit::app::widgets::playhead::TICK_MS;
use rust_cli_tui_kit::app::widgets::{fade, key_hints};
use rust_cli_tui_kit::markdown::render::SETTLED;
use rust_cli_tui_kit::render::Span;

use super::app::{Action, App, Overlay};
use super::cards::Card;
use super::context_page::{self, Inputs};
use super::context_tree::{self, Move, Path};
use super::page_frame::PageBody;
use super::stats_page::{self, Tab};

/// 最近一次的答案（与它问的会话）、失败的原因与页上每个数变了的时刻。
#[derive(Debug, Default)]
pub struct ContextCount {
    /// 问的会话（None 是还没有会话：按新会话的模型与模式算）。
    answer: Option<(Option<SessionId>, ContextUsage)>,
    /// 最近一次失败（答案还没有时页上代替 `Counting…`）。
    failed: Option<String>,
    /// 页上的数变了的那一帧（键同 `context_page::figures`）：淡入从那一刻起。
    changed: BTreeMap<String, (u64, Instant)>,
}

/// 页的 Context 标签（`stats_page::StatsPage.context`）：开着的行、选中的行（路径）与顶行。打开时全部合着、选第一行。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextPage {
    pub open: BTreeSet<Path>,
    pub chosen: Path,
    pub top: usize,
}

impl App {
    /// 主会话（还没有会话时 None）。
    fn context_session(&self) -> Option<SessionId> {
        self.session.as_deref().map(SessionId::new)
    }

    /// 要不要问：卡或页开着、看的是主会话时问它（外层 None 是不问；内层 None 是还没有会话）。
    pub fn context_wanted(&self) -> Option<Option<SessionId>> {
        if self.viewing.is_some()
            || !matches!(
                self.overlay,
                Some(Overlay::Card(Card::Stats(_) | Card::StatsPage(_)))
            )
        {
            return None;
        }
        Some(self.context_session())
    }

    /// 现在看的会话的答案（查看子代理时没有）。
    pub fn context_now(&self) -> Option<&ContextUsage> {
        let (session, usage) = self.context_count.answer.as_ref()?;
        (self.viewing.is_none() && *session == self.context_session()).then_some(usage)
    }

    /// 答案回来。问的不是现在看的会话时丢掉，返回 true（runner 按现在的再问一次）。数变了的记下时刻（第一次的答案不淡）。
    pub fn context_counted(
        &mut self,
        session: Option<SessionId>,
        result: Result<ContextUsage, String>,
        now: Instant,
        now_ms: u64,
    ) -> bool {
        if self.viewing.is_some() || session != self.context_session() {
            return true;
        }
        match result {
            Ok(usage) => {
                if let Some(before) = self.context_now() {
                    let before: BTreeMap<String, u64> =
                        context_page::figures(before).into_iter().collect();
                    for (key, value) in context_page::figures(&usage) {
                        if before.get(&key) != Some(&value) {
                            self.context_count.changed.insert(key, (now_ms, now));
                        }
                    }
                }
                self.context_count.answer = Some((session, usage));
                self.context_count.failed = None;
            }
            Err(reason) => self.context_count.failed = Some(reason),
        }
        false
    }

    /// 卡的组成（kit `stats::Window.breakdown`）：没有答案时空着（卡没有图例）。
    pub fn context_breakdown(&self) -> Vec<(String, u64)> {
        self.context_now()
            .map(context_tree::breakdown)
            .unwrap_or_default()
    }

    /// 页上的数在淡入。
    pub(super) fn context_fading(&self) -> bool {
        let ticks = u128::from(TICK_MS) * u128::from(fade::TICKS.unsigned_abs());
        self.context_count
            .changed
            .values()
            .any(|(_, at)| at.elapsed().as_millis() < ticks)
    }

    /// 规则文件的路径相对于工作区、家目录写成 `~`。
    fn context_places(&self) -> context_tree::Places<'_> {
        context_tree::Places {
            root: self.workspace(),
            home: self.home.as_deref(),
        }
    }

    fn context_lines(&self, page: &ContextPage, width: usize, now_ms: u64) -> context_page::Page {
        let changed = &self.context_count.changed;
        let age = |key: &str| {
            changed.get(key).map_or(SETTLED, |(at, _)| {
                i64::try_from(now_ms.saturating_sub(*at) / TICK_MS).unwrap_or(SETTLED)
            })
        };
        let fallback = self.stats_usage();
        context_page::page(&Inputs {
            theme: &self.ink.theme,
            width,
            crumb: stats_page::crumb(&self.ink.theme, width, Tab::Context, false),
            usage: self.context_now(),
            fallback: &fallback,
            failed: self.context_count.failed.as_deref(),
            open: &page.open,
            chosen: &page.chosen,
            age: &age,
            places: self.context_places(),
        })
    }

    /// Context 标签（`stats_page_body`）。槽：能开的行 `⏎ expand`，开着的 `⏎ collapse`；后面是 `⇥ session`。
    pub(super) fn context_page_body(
        &self,
        page: &ContextPage,
        width: usize,
        now_ms: u64,
    ) -> PageBody {
        let lines = self.context_lines(page, width, now_ms).lines;
        let open = self.context_now().and_then(|usage| {
            let nodes = context_tree::outline(usage, &self.context_places());
            let rows = context_tree::visible(&nodes, &page.open);
            let at = context_tree::chosen_at(&rows, &page.chosen);
            rows.get(at).and_then(|(_, shown)| shown.open)
        });
        let mut keys: Vec<(String, String)> = match open {
            Some(false) => vec![("⏎".to_owned(), "expand".to_owned())],
            Some(true) => vec![("⏎".to_owned(), "collapse".to_owned())],
            None => Vec::new(),
        };
        keys.push(("⇥".to_owned(), "session".to_owned()));
        let slot: Vec<Span> = key_hints::spans(&self.ink.theme, &key_hints::pick(&keys));
        PageBody {
            lines,
            top: page.top,
            slot: Some(slot),
            owns_keys: false,
            fit: false,
        }
    }

    /// Context 标签的键（§4.3）：↑↓、PgUp/PgDn、Home/End 移动选中，⏎ 开合，→ 打开，← 合上或去上一层；其余的键去草稿
    /// （esc 与 ⇥ 归页，`stats_page_key`）。页跟着选中滚：选中的行离页的上下边至少一行。
    pub(super) fn context_page_key(&mut self, key: KeyEvent) -> Option<Option<Action>> {
        let mut page = self.stats_page()?.context.clone();
        let (rows, _) = self.page_rows;
        let screen = rows.saturating_sub(1).max(1) as isize;
        let how = match key.code {
            KeyCode::Up => Move::By(-1),
            KeyCode::Down => Move::By(1),
            KeyCode::PageUp => Move::By(-screen),
            KeyCode::PageDown => Move::By(screen),
            KeyCode::Home => Move::First,
            KeyCode::End => Move::Last,
            KeyCode::Enter => Move::Toggle,
            KeyCode::Right => Move::Open,
            KeyCode::Left => Move::Close,
            _ => return None,
        };
        if let Some(usage) = self.context_now() {
            let nodes = context_tree::outline(usage, &self.context_places());
            context_tree::step(&nodes, &mut page.open, &mut page.chosen, how);
            let (width, now_ms) = self.page_at;
            let drawn = self.context_lines(&page, width.max(1), now_ms);
            if let Some(at) = drawn.chosen {
                page.top = follow(page.top, at, rows, drawn.lines.len());
            }
        }
        if let Some(Overlay::Card(Card::StatsPage(stats))) = self.overlay.as_mut() {
            stats.context = page;
        }
        Some(None)
    }
}

/// 选中的行离一屏的上下边至少这么多行（边上那一行可能是 `↑ n more`）。
const MARGIN: usize = 1;

/// 让第 `at` 行看得见的顶行：离一屏（`rows` 行）的上下边至少 [`MARGIN`] 行，夹到最后一屏。
fn follow(top: usize, at: usize, rows: usize, total: usize) -> usize {
    let max_top = total.saturating_sub(rows);
    let top = top.min(max_top);
    let top = if at < top + MARGIN {
        at.saturating_sub(MARGIN)
    } else if rows > 2 * MARGIN && at + MARGIN + 1 > top + rows {
        at + MARGIN + 1 - rows
    } else {
        top
    };
    top.min(max_top)
}

#[cfg(test)]
#[path = "context_count_tests.rs"]
mod tests;
