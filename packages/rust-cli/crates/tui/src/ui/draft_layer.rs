//! 跟着草稿的一层（docs/specs/rust-cli/tui-kit.md §4.0「板」；Puffin `settle_picker`、`settle_modes`、`settle_themes`、
//! `settle_rewind_list` 与 `sheet_open`）：列表开着时草稿是它的命令（`/model `、`/mode ` …），打字进草稿、按草稿里命令后的
//! 第一个词过滤（可过滤的列表）；草稿不再以命令开头时关上；esc 关上、草稿留着，⏎ 再发出命令就又打开。`?` 按键表开着当且
//! 仅当草稿正好是打出的 `?`、esc 没有关上它；⏎ 关上并清空草稿。
//!
//! 以前列表自己拿所有的键：顶上一行过滤输入、底下一行按键提示，esc 关上；`?` 不进草稿，直接打开按键表、任何键关上。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};

use super::app::{Action, App, Overlay};
use super::list::{ListView, Purpose};

/// 列表跟着的命令（草稿以它开头）；没有的列表自己拿键（`/memory` 等）。
pub fn prefix(purpose: &Purpose) -> Option<&'static str> {
    match purpose {
        Purpose::Model => Some("/model "),
        Purpose::Permissions => Some("/mode "),
        Purpose::Theme => Some("/theme "),
        Purpose::Export => Some(super::export::EXPORT_DRAFT),
        Purpose::Info => None,
    }
}

/// 草稿里命令后的第一个词（Puffin `model_words` 的第一个）。
fn first_word(text: &str, prefix: &str) -> String {
    let rest = text.get(prefix.len()..).unwrap_or_default();
    rest.split(' ').next().unwrap_or_default().to_owned()
}

/// 草稿以 `prefix` 开头（不分大小写）。
fn starts(text: &str, prefix: &str) -> bool {
    text.get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

impl App {
    /// 打开一个列表：跟着草稿的列表把草稿换成它的命令（草稿空着时；从 runner 晚到的列表不盖掉已经打的字）。草稿已经是
    /// `/model 词` 时（`/model 词` ⏎，或目录到之前接着打的字）当场按词过滤：过滤以前只在按键之后定，晚到的列表要等下一个键。
    pub fn show_list(&mut self, mut list: ListView) {
        if let Some(prefix) = prefix(&list.purpose) {
            if self.composer.is_empty() {
                self.composer.set_text(prefix);
            }
            let text = self.composer.area.text();
            if let Some(filter) = list.filter.as_mut()
                && starts(text, prefix)
            {
                *filter = first_word(text, prefix);
            }
        }
        self.overlay = Some(Overlay::List(list));
    }

    /// 命令架上 ⇥（`name` 可以是别名，草稿换成 `/model `）、或打出的空格让草稿正好成了 `/model `、`/theme `（Puffin `shelf_key`
    /// 与 `settle_picker`，tui-kit.md §4.0「板」）：不等 ⏎，打开它的列表。不是这两个命令、或命令此刻不能运行（回合中的
    /// `/model`）时返回 None，草稿照旧只是编辑。
    pub(super) fn open_by_draft(&mut self, name: &str) -> Option<Option<Action>> {
        let command = super::commands::find(name)?;
        if !matches!(command.name, "model" | "theme")
            || super::shelf_rows::unavailable(self, command.name).is_some()
        {
            return None;
        }
        self.composer.popup = None;
        self.composer.set_text(&format!("/{} ", command.name));
        Some(match command.name {
            "model" => self.request_models(),
            _ => self.theme_list(),
        })
    }

    /// 请 runner 读模型目录、打开模型列表；回答还没到时不再请，到了的那一次按当时的草稿过滤（`show_list`）。修的竞态：打出
    /// `/model ` 已经在读目录，目录到之前接着打字、按 ⏎ 又读一次；第一次的列表上选中之后第二次的回答才到，选择器又开了。
    pub(super) fn request_models(&mut self) -> Option<Action> {
        (!std::mem::replace(&mut self.models_opening, true)).then_some(Action::OpenModels)
    }

    /// 开着的是跟着草稿的一层（列表或按键表）：键先给它，不拿的去编辑草稿。
    pub(super) fn drafted(&self) -> bool {
        match &self.overlay {
            Some(Overlay::List(list)) => prefix(&list.purpose).is_some(),
            Some(Overlay::Shortcuts)
            | Some(Overlay::Card(_))
            | Some(Overlay::Sessions(_))
            | Some(Overlay::Points(_))
            | Some(Overlay::Forks(_)) => true,
            _ => false,
        }
    }

    /// 跟着草稿的一层拿的键：`Some` 到此为止，`None` 让键去编辑草稿。列表拿 esc、↑↓、ctrl+p/n、⏎（模型列表还有 ←→）；
    /// 按键表拿 esc 与 ⏎（Puffin `picker_key`、`modes_key`、`sheet_key`）。不过滤的列表还拿数字 1–9 直选（旧界面有、Puffin
    /// 没有的能力，§4.0「旧界面独有」）。
    pub(super) fn drafted_key(&mut self, key: KeyEvent, now: Instant) -> Option<Option<Action>> {
        let ctrl = crate::ui::keyboard::ctrl(&key);
        match self.overlay.as_ref()? {
            // 输入层的卡（cards.rs）。
            Overlay::Card(_) => self.card_key(key),
            // `/resume` 的会话列表（sessions_list.rs）。
            Overlay::Sessions(_) => self.sessions_key(key),
            // `/rewind` 的点列表（rewind.rs）。
            Overlay::Points(_) => self.points_key(key),
            // `/fork` 的点列表（fork.rs）。
            Overlay::Forks(_) => self.forks_key(key),
            Overlay::Shortcuts => match key.code {
                KeyCode::Esc => {
                    self.keys_closed = true;
                    self.overlay = None;
                    Some(None)
                }
                KeyCode::Enter => {
                    self.composer.clear();
                    self.overlay = None;
                    Some(None)
                }
                _ => None,
            },
            Overlay::List(list) => {
                let model = list.purpose == Purpose::Model;
                let digits = list.filter.is_none();
                let takes = match key.code {
                    KeyCode::Esc | KeyCode::Up | KeyCode::Down | KeyCode::Enter => true,
                    KeyCode::Char('p' | 'n') => ctrl,
                    KeyCode::Char('1'..='9') => digits && !ctrl,
                    KeyCode::Left | KeyCode::Right => model,
                    _ => false,
                };
                takes.then(|| self.overlay_key(key, now))
            }
            _ => None,
        }
    }

    /// 选中了跟着草稿的列表里的一项：草稿清空（Puffin 选中后 `Draft::default()`）。
    pub(super) fn drop_list_draft(&mut self, purpose: &Purpose) {
        if prefix(purpose).is_some() {
            self.composer.clear();
        }
    }

    /// 一个键或一次粘贴之后（草稿原来是 `before`；`recall`：草稿是从历史调回的）：按草稿开关按键表、关上或过滤列表。
    /// 会话列表的词变了时返回给 runner 的搜索。
    pub(super) fn settle_drafted(&mut self, before: &str, recall: bool) -> Option<Action> {
        let text = self.composer.area.text().to_owned();
        if text != before {
            self.keys_closed = recall && text == "?";
        }
        let sheet = text == "?"
            && text != before
            && !self.keys_closed
            && self.viewing.is_none()
            && self.pending().is_none()
            && self.view.peer_held().is_none();
        match self.overlay.as_mut() {
            Some(Overlay::Shortcuts) if text != "?" => self.overlay = None,
            Some(Overlay::Sessions(_)) => return self.settle_sessions(&text),
            Some(Overlay::Points(_)) => self.settle_points(&text),
            Some(Overlay::Forks(_)) => self.settle_forks(&text),
            Some(Overlay::List(list)) => {
                let prefix = prefix(&list.purpose)?;
                // `/export` 的列表只在草稿正好是 `/export ` 时开着（Puffin `export_open`）：选了存成文件，草稿成了
                // `/export 文件名`，列表关上。
                let exact = list.purpose == Purpose::Export;
                if !starts(&text, prefix) || (exact && !text.eq_ignore_ascii_case(prefix)) {
                    self.overlay = None;
                } else if let Some(filter) = list.filter.as_mut() {
                    let word = first_word(&text, prefix);
                    if *filter != word {
                        *filter = word;
                        list.selected = 0;
                    }
                }
            }
            None if sheet => self.overlay = Some(Overlay::Shortcuts),
            _ => {}
        }
        None
    }
}

#[cfg(test)]
#[path = "draft_layer_tests.rs"]
mod tests;
