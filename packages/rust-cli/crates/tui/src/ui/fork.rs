//! `/fork` 的点列表（docs/specs/rust-cli/session-lifecycle.md §6.5.3，工作线 F75；tui-kit.md §4.3「`/fork`」）：Puffin 没有
//! `/fork`，照它的 `/rewind` 列表那一层画（`rewind.rs`，kit 的 `rewind_rows::list_rows_said`）：板上输入框上方，跟着草稿
//! `/fork <词>`，最新的在上，每行是年龄与消息的第一行，选中的点下面一行说分叉做什么。↑↓ 选、⏎ 在那条输入之后分叉（没有
//! 第二步）、esc 关上（草稿留着）。可选的输入与 `/rewind` 同一份（内核给出）。
//!
//! 2026-10-31 以前是旧界面的列表：标题 `Fork`、一句副标题、旧的在上、数字键直选。
use crossterm::event::{KeyCode, KeyEvent};
use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::session_api::Forked;
use rust_cli_tui_kit::app::element::Element;
use rust_cli_tui_kit::app::widgets::rewind_rows::{self, List, Point, PointFrom};
use rust_cli_tui_kit::render::Span;

use super::app::{Action, App, Overlay};
use super::i18n::Lang;
use super::keyboard;
use super::pager::cycle;
use crate::view::Body;

/// 列表跟着的草稿。
pub const DRAFT: &str = "/fork ";

/// 一条可选的输入：消息 ID 与它的文字。
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub message: MessageId,
    pub text: String,
}

/// 点列表：可选的输入（会话里的顺序，旧的在前）、过滤词与选中的行（同 `rewind::PointsList`）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ForkList {
    pub entries: Vec<Entry>,
    pub query: String,
    pub choice: usize,
}

/// 分叉完成后的提示（§6.5.3）：英文是内核的回执（Node 原文）加 Node 的 `Switched to forked session <ID>.`；
/// Node 只有英文，中文另配（工作线 F78），消息 ID 与条数取回执的 `through`、`copied`。
pub fn forked_notice(lang: Lang, forked: &Forked) -> String {
    let session = &forked.session;
    let through = forked
        .through
        .as_ref()
        .map(MessageId::as_str)
        .unwrap_or_default();
    match lang {
        Lang::En => format!(
            "{}\nSwitched to forked session {session}.",
            forked.response().unwrap_or_default()
        ),
        Lang::Zh => format!(
            "已从消息 {through} 分出会话 {session}：复制了 {} 条消息。\n已切换到分出的会话 {session}。",
            forked.copied
        ),
    }
}

/// 草稿里 `/fork ` 之后的词（同 `rewind` 的 `query_of`）。
fn query_of(text: &str) -> String {
    text.chars()
        .skip(DRAFT.len())
        .collect::<String>()
        .trim()
        .to_owned()
}

impl App {
    fn fork_list(&self) -> Option<&ForkList> {
        match &self.overlay {
            Some(Overlay::Forks(list)) => Some(list),
            _ => None,
        }
    }

    /// 点在 kit 里的样子：都能分叉（不变暗），没有文件的统计；`at` 是你的那块在视图里出现的时刻（同 `rewind_pick::kit_points`）。
    fn fork_points(&self, list: &ForkList) -> Vec<Point> {
        let mut users = self
            .view
            .rows()
            .iter()
            .filter(|row| matches!(row.body, Body::User { .. }));
        (0..list.entries.len())
            .map(|_| Point {
                at: users.next().map_or(0, |row| row.created_ms),
                from: PointFrom::Answer,
                added: 0,
                removed: 0,
            })
            .collect()
    }

    /// 列表为现在的词显示的点（最新的在前）。
    fn fork_matches(&self, list: &ForkList) -> Vec<usize> {
        let points = self.fork_points(list);
        rewind_rows::matches(&points, &|k| list.entries[k].text.clone(), &list.query)
    }

    /// 输入到了（`/fork`）：没有可以分叉的时输入框上方说一句、不开（Puffin `rewind_blocked` 的 `Nothing to rewind yet.`）；
    /// 否则草稿空着时写成 `/fork `，打开列表。
    pub fn open_fork(&mut self, entries: Vec<Entry>) {
        if entries.is_empty() {
            let text = self
                .lang
                .pick("Nothing to fork from yet.", "还没有可以分叉的输入。");
            self.info(text);
            return;
        }
        if self.composer.is_empty() {
            self.composer.set_text(DRAFT);
        }
        let query = query_of(self.composer.area.text());
        self.overlay = Some(Overlay::Forks(ForkList {
            entries,
            query,
            choice: 0,
        }));
    }

    /// 列表拿的键（`draft_layer.rs` 先问它）：`None` 让键去编辑草稿。
    pub(super) fn forks_key(&mut self, key: KeyEvent) -> Option<Option<Action>> {
        let ctrl = keyboard::ctrl(&key);
        let list = self.fork_list()?.clone();
        let ms = self.fork_matches(&list);
        let n = ms.len();
        let mut choice = list.choice;
        match key.code {
            KeyCode::Esc => {
                self.overlay = None;
                return Some(None);
            }
            KeyCode::Up if n > 0 => choice = cycle(choice.min(n - 1), n, false),
            KeyCode::Char('p') if ctrl && n > 0 => choice = cycle(choice.min(n - 1), n, false),
            KeyCode::Down if n > 0 => choice = cycle(choice.min(n - 1), n, true),
            KeyCode::Char('n') if ctrl && n > 0 => choice = cycle(choice.min(n - 1), n, true),
            KeyCode::Up | KeyCode::Down | KeyCode::Tab => {}
            KeyCode::Char('p' | 'n') if ctrl => {}
            KeyCode::Enter if n == 0 => {}
            KeyCode::Enter => {
                let input = list.entries[ms[choice.min(n - 1)]].message.clone();
                self.overlay = None;
                self.composer.clear();
                return Some(Some(Action::Fork { input }));
            }
            _ => return None,
        }
        if let Some(Overlay::Forks(list)) = self.overlay.as_mut() {
            list.choice = choice;
        }
        Some(None)
    }

    /// 一个键或粘贴之后：草稿不再以 `/fork ` 开头时关上；词变了时选第一行。
    pub(super) fn settle_forks(&mut self, text: &str) {
        let starts = text
            .get(..DRAFT.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(DRAFT));
        if !starts {
            self.overlay = None;
            return;
        }
        let q = query_of(text);
        if let Some(Overlay::Forks(list)) = self.overlay.as_mut()
            && list.query != q
        {
            list.query = q;
            list.choice = 0;
        }
    }

    /// 列表的行（kit `list_rows_said`）。`now_ms` 是墙钟。
    pub fn forks_rows(&self, now_ms: u64) -> Vec<Element> {
        let Some(list) = self.fork_list() else {
            return Vec::new();
        };
        let theme = &self.ink.theme;
        let lang = self.lang;
        let points = self.fork_points(list);
        let none = match lang {
            Lang::En => format!("No message matches “{}”.", list.query),
            Lang::Zh => format!("没有相符的消息“{}”。", list.query),
        };
        let ui = List {
            query: &list.query,
            choice: list.choice,
        };
        let detail = |_k: usize| {
            vec![
                Span::new(
                    lang.pick("a new session after this turn", "在这一回合之后分出新会话"),
                    theme.fg3,
                ),
                Span::new("  ·  ", theme.line),
                Span::new(lang.pick("files are not changed", "文件不动"), theme.fg3),
            ]
        };
        rewind_rows::list_rows_said(
            theme,
            &points,
            &|k| list.entries[k].text.clone(),
            &ui,
            (now_ms as f64, &*self.zone),
            &none,
            &detail,
        )
    }
}

#[cfg(test)]
#[path = "fork_tests.rs"]
mod tests;
