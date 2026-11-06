//! `/rewind` 的点列表（docs/puffin-tui.md §4.2「回退（/rewind）」；tui-kit.md §4.2「`/rewind`」；Puffin `shell/rewind.rs` 的
//! `rewind_list_open`、`settle_rewind_list`、`rewind_list_key`、`rewind_list_rows`）：板上输入框上方的一层，跟着草稿
//! `/rewind <词>`。行是 kit 的 `rewind_rows::list_rows`（最新的在上，年龄、消息的第一行、选中的点的细节）；↑↓ 选、⏎ 在那个
//! 点上开始挑选（`rewind_pick.rs` 的 `pick_point`）、esc 关上（草稿留着）。
//!
//! 2026-10-31 以前是旧界面的列表（旧的在上、末行 `(current)`）与「Restore code and conversation」的确认页；确认页删掉，回退
//! 是 Puffin 的分支（fork）。旧界面的原地回退（内核的 `Rewind`，能只恢复代码）留在列表的 ⌃r 后面：板上的一问（kit 请求的
//! 头行与编号的回答）`Rewind in place to before your 2nd message?`，`1 Conversation`、`2 Code`、`3 Both`（这个点之后没有
//! 可以恢复的代码时只有 1），esc 回到列表。键只写在 `?` 表（Puffin 的列表槽空着；tui-kit.md §4.3）。
use crossterm::event::{KeyCode, KeyEvent};
use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::session_api::{RewindOption, RewindScope};
use rust_cli_tui_kit::app::element::Element;
use rust_cli_tui_kit::app::widgets::asks::{
    Answer as KitAnswer, AnswerKind, answer_rows, head_row_said,
};
use rust_cli_tui_kit::app::widgets::rewind_rows;
use rust_cli_tui_kit::render::Span;

use super::app::{Action, App, Overlay};
use super::doorway::{Answer, answers};
use super::i18n::Lang;
use super::keyboard;
use super::pager::cycle;

/// 列表跟着的草稿。
pub const DRAFT: &str = "/rewind ";

/// 一条可以回去的你的消息：内核给的改动统计、消息文字、它上面最近的一条回答（在它上面 fork；没有时第一条回到开头，
/// 其余回不去，Puffin host 的 `UserMark::answer`）。
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub option: RewindOption,
    pub text: String,
    pub answer: Option<MessageId>,
}

/// 内核的可选点与会话的消息合成列表的点（runner 与孪生共用这一条规则）。
pub fn entries_of(options: Vec<RewindOption>, messages: &[Message]) -> Vec<Entry> {
    let mut last_answer = None;
    let mut above = std::collections::HashMap::new();
    for message in messages {
        match message {
            Message::Assistant(a) => last_answer = Some(a.id.clone()),
            Message::User(_) => {
                above.insert(message.id().clone(), last_answer.clone());
            }
            _ => {}
        }
    }
    options
        .into_iter()
        .map(|option| {
            let text = messages
                .iter()
                .find(|m| m.id() == &option.message)
                .and_then(super::session_rows::user_text)
                .unwrap_or_default();
            let answer = above.get(&option.message).cloned().flatten();
            Entry {
                option,
                text,
                answer,
            }
        })
        .collect()
}

/// 草稿里 `/rewind ` 之后的词（Puffin `rewind_query`）。
fn query_of(text: &str) -> String {
    text.chars()
        .skip(DRAFT.len())
        .collect::<String>()
        .trim()
        .to_owned()
}

/// 点列表的过滤词与选中的行（Puffin `RewindList`）；`in_place` 是 ⌃r 的一问：哪个点与选中的回答。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PointsList {
    pub query: String,
    pub choice: usize,
    pub in_place: Option<(usize, usize)>,
}

impl PointsList {
    /// 在原地回退的一问上（它拿键，槽是 `esc back`）。
    pub fn asking(&self) -> bool {
        self.in_place.is_some()
    }
}

impl App {
    fn points_list(&self) -> Option<&PointsList> {
        match &self.overlay {
            Some(Overlay::Points(list)) => Some(list),
            _ => None,
        }
    }

    /// 点 `k` 的消息。
    fn entry_text(&self, k: usize) -> String {
        self.rewind
            .get(k)
            .map(|e| e.text.clone())
            .unwrap_or_default()
    }

    /// 列表为现在的词显示的点（最新的在前）。
    fn point_matches(&self, query: &str) -> Vec<usize> {
        let points = super::rewind_pick::kit_points(self);
        rewind_rows::matches(&points, &|k| self.entry_text(k), query)
    }

    /// 点到了（`/rewind`）：草稿空着时写成 `/rewind `，打开列表。esc 连按两次时在最后一个点上开始挑选，不开列表。
    pub fn open_rewind(&mut self, entries: Vec<Entry>) {
        if std::mem::take(&mut self.picking_wanted) {
            let last = entries.len().checked_sub(1);
            self.rewind = entries;
            if let Some(last) = last {
                self.pick_point(last);
            }
            return;
        }
        self.rewind = entries;
        if self.composer.is_empty() {
            self.composer.set_text(DRAFT);
        }
        let query = query_of(self.composer.area.text());
        self.overlay = Some(Overlay::Points(PointsList {
            query,
            choice: 0,
            in_place: None,
        }));
    }

    /// 点 `k` 原地回退能选的范围：对话总有；这个点之后有可以恢复的代码时还有代码与两者。
    fn scopes(&self, k: usize) -> Vec<RewindScope> {
        let code = self.rewind.get(k).is_some_and(|e| e.option.code);
        if code {
            vec![
                RewindScope::Conversation,
                RewindScope::Code,
                RewindScope::Both,
            ]
        } else {
            vec![RewindScope::Conversation]
        }
    }

    /// 原地回退那一问拿的键：↑↓ 与数字选、⏎ 回退、esc 回到列表。
    fn in_place_key(&mut self, key: &KeyEvent, k: usize, choice: usize) -> Option<Action> {
        let set = |app: &mut Self, v: Option<(usize, usize)>| {
            if let Some(Overlay::Points(list)) = app.overlay.as_mut() {
                list.in_place = v;
            }
        };
        if key.code == KeyCode::Esc {
            set(self, None);
            return None;
        }
        let scopes = self.scopes(k);
        let ctrl = keyboard::ctrl(key);
        match answers(key, ctrl, scopes.len(), choice.min(scopes.len() - 1), true) {
            Answer::Move(i) => set(self, Some((k, i))),
            Answer::Act(i) => {
                let entry = self.rewind.get(k)?;
                let action = Action::Rewind {
                    target: entry.option.message.clone(),
                    scope: scopes[i],
                    text: entry.text.clone(),
                };
                self.overlay = None;
                self.composer.clear();
                return Some(action);
            }
            Answer::Pass => {}
        }
        None
    }

    /// 列表拿的键（`draft_layer.rs` 先问它）：`None` 让键去编辑草稿。
    pub(super) fn points_key(&mut self, key: KeyEvent) -> Option<Option<Action>> {
        let ctrl = keyboard::ctrl(&key);
        let list = self.points_list()?.clone();
        if let Some((k, at)) = list.in_place {
            return Some(self.in_place_key(&key, k, at));
        }
        let ms = self.point_matches(&list.query);
        let n = ms.len();
        let mut choice = list.choice;
        match key.code {
            // 原地回退（旧界面的能力，tui-kit.md §4.3）：选中的点上问回退什么。
            KeyCode::Char('r') if ctrl && n > 0 => {
                let k = ms[choice.min(n - 1)];
                if let Some(Overlay::Points(list)) = self.overlay.as_mut() {
                    list.in_place = Some((k, 0));
                }
                return Some(None);
            }
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
            KeyCode::Enter if n == 0 || self.running() => {}
            KeyCode::Enter => {
                let k = ms[choice.min(n - 1)];
                // 回不去的点（上面的回合没有完成）：⏎ 不做（Puffin `pick_at` 之后槽里说为什么）。
                self.overlay = None;
                self.pick_point(k);
                return Some(None);
            }
            _ => return None,
        }
        if let Some(Overlay::Points(list)) = self.overlay.as_mut() {
            list.choice = choice;
        }
        Some(None)
    }

    /// 一个键或粘贴之后：草稿不再以 `/rewind ` 开头时关上；词变了时选第一行。
    pub(super) fn settle_points(&mut self, text: &str) {
        let starts = text
            .get(..DRAFT.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(DRAFT));
        if !starts {
            self.overlay = None;
            return;
        }
        let q = query_of(text);
        if let Some(Overlay::Points(list)) = self.overlay.as_mut()
            && list.query != q
        {
            list.query = q;
            list.choice = 0;
        }
    }

    /// 列表的行（kit `list_rows`），在原地回退那一问上时是它的行。`now_ms` 是墙钟。
    pub(super) fn points_rows(&self, now_ms: u64) -> Vec<Element> {
        let Some(list) = self.points_list() else {
            return Vec::new();
        };
        let points = super::rewind_pick::kit_points(self);
        if let Some((k, choice)) = list.in_place {
            return self.in_place_rows(&points, k, choice);
        }
        let ui = rewind_rows::List {
            query: &list.query,
            choice: list.choice,
        };
        rewind_rows::list_rows(
            &self.ink.theme,
            &points,
            &|k| self.entry_text(k),
            &ui,
            now_ms as f64,
            &*self.zone,
        )
    }
}

impl App {
    /// 原地回退那一问的行：头行与编号的回答（同 `/resume` 的删除那一问，sessions_steps.rs）。
    fn in_place_rows(
        &self,
        points: &[rewind_rows::Point],
        k: usize,
        choice: usize,
    ) -> Vec<Element> {
        let theme = &self.ink.theme;
        let zh = self.lang == Lang::Zh;
        let head = if zh {
            format!("原地回退到你的第 {} 条消息之前？", k + 1)
        } else {
            format!(
                "Rewind in place to before your {} message?",
                rewind_rows::ordinal(k + 1)
            )
        };
        let (added, removed) = points.get(k).map_or((0, 0), |p| (p.added, p.removed));
        let words = |en: &str, cn: &str| if zh { cn.to_owned() } else { en.to_owned() };
        let answer = |id: &str, label: String, desc: Vec<Span>| KitAnswer {
            id: id.to_owned(),
            kind: AnswerKind::Keep,
            label,
            desc: desc
                .into_iter()
                .map(|s| (s.text.to_string(), s.style))
                .collect(),
        };
        let fg3 = |t: String| vec![Span::new(t, theme.fg3)];
        let mut code = fg3(words("files are restored", "文件恢复"));
        let change = rewind_rows::change(theme, added, removed);
        if !change.is_empty() {
            code.push(Span::new(" ", theme.fg3));
            code.extend(change);
        }
        let all = [
            answer(
                "conversation",
                words("Conversation", "对话"),
                fg3(words("turns are dropped", "之后的回合删掉")),
            ),
            answer("code", words("Code", "代码"), code),
            answer("both", words("Both", "两者"), Vec::new()),
        ];
        let n = self.scopes(k).len();
        let mut rows = vec![head_row_said(theme, &head, None, None)];
        rows.extend(answer_rows(theme, &all[..n], choice.min(n - 1), 1.0));
        rows
    }
}

#[cfg(test)]
#[path = "rewind_tests.rs"]
mod tests;
