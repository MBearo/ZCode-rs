//! 设置页的按键（docs/specs/rust-cli/tui-kit.md §4.2「设置」；照 Puffin `shell/settings/keys.rs`）。目录：↑ ↓ 换段（绕回），
//! → 或 ⏎ 进页，esc 关上；页：↑ ↓ 选行，← 回目录（窄时 ← → 换段），esc 一级一级退（清搜索、退页、关上），⏎ 改或打开，空格
//! 开关，⌫ 删规则，打字是搜索。有请求在等时 esc 直接关上回去回答它。
use crossterm::event::{KeyCode, KeyEvent};
use rust_cli_kernel_api::provider::CatalogModel;
use rust_cli_tui_kit::app::widgets::settings::{Applies, NARROW, SECTIONS};

use super::field::Field;
use super::page::{Act, Change, ItemRow, Row, Write};
use super::{Mode, Settings};
use crate::ui::app::{Action, App};

/// `n` 个里从 `i` 走 `by` 步，绕回。
fn wrapped(i: usize, by: i64, n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    (i as i64 + by).rem_euclid(n as i64) as usize
}

/// 选择器给 `m` 显示的档位：← → 设过而它有的，否则它最高的。
pub fn effort_of(m: &CatalogModel, wish: &str) -> String {
    if m.reasoning_levels.iter().any(|e| e == wish) {
        wish.to_owned()
    } else {
        m.reasoning_levels.last().cloned().unwrap_or_default()
    }
}

/// 一个键在设置页里的意思（Puffin 的 `Key`，只留设置页认的）。
#[derive(Clone, Debug, PartialEq, Eq)]
enum Pressed {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Esc,
    Space,
    Backspace,
    Delete,
    Home,
    End,
    CtrlC,
    CtrlU,
    CtrlK,
    CtrlW,
    WordLeft,
    WordRight,
    Text(String),
    Other,
}

impl App {
    fn settings_k(&self, key: &KeyEvent) -> Pressed {
        let ctrl = crate::ui::keyboard::ctrl(key);
        let alt = crate::ui::keyboard::alt(key);
        match key.code {
            KeyCode::Up => Pressed::Up,
            KeyCode::Down => Pressed::Down,
            KeyCode::Left if ctrl || alt => Pressed::WordLeft,
            KeyCode::Right if ctrl || alt => Pressed::WordRight,
            KeyCode::Left => Pressed::Left,
            KeyCode::Right => Pressed::Right,
            KeyCode::Enter => Pressed::Enter,
            KeyCode::Esc => Pressed::Esc,
            KeyCode::Backspace if alt => Pressed::CtrlW,
            KeyCode::Backspace => Pressed::Backspace,
            KeyCode::Delete => Pressed::Delete,
            KeyCode::Home => Pressed::Home,
            KeyCode::End => Pressed::End,
            KeyCode::Char(c) if ctrl => match c.to_ascii_lowercase() {
                'p' => Pressed::Up,
                'n' => Pressed::Down,
                'b' => Pressed::Left,
                'f' => Pressed::Right,
                'a' => Pressed::Home,
                'e' => Pressed::End,
                'h' => Pressed::Backspace,
                'd' => Pressed::Delete,
                'c' => Pressed::CtrlC,
                'u' => Pressed::CtrlU,
                'k' => Pressed::CtrlK,
                'w' => Pressed::CtrlW,
                _ => Pressed::Other,
            },
            KeyCode::Char(' ') => Pressed::Space,
            KeyCode::Char(c) => Pressed::Text(c.to_string()),
            _ => Pressed::Other,
        }
    }

    /// 设置页开着时的一个键（`keys.rs` 在选区之后先问这里）。
    pub(in crate::ui) fn settings_key(&mut self, key: KeyEvent) -> Option<Action> {
        let k = self.settings_k(&key);
        self.settings_input(k)
    }

    /// 设置页开着时的粘贴：编辑时进值，否则进搜索。
    pub(in crate::ui) fn settings_paste(&mut self, text: &str) -> Option<Action> {
        self.settings_input(Pressed::Text(text.replace("\r\n", " ").replace('\n', " ")))
    }

    fn settings_input(&mut self, key: Pressed) -> Option<Action> {
        let ui = self.settings.clone()?;
        let narrow = ui.width < NARROW;
        // 有请求在等：esc 直接关上，回去回答它。
        if self.interaction.is_some() && key == Pressed::Esc {
            self.settings_close();
            return None;
        }
        match &ui.mode {
            Mode::Editing { key: k, draft } => self.settings_edit_key(&key, k, draft),
            Mode::Choosing { key: k, at } => self.settings_choosing_key(&ui, &key, k, *at),
            Mode::Confirming { .. } => self.settings_confirm_key(&key),
            Mode::Picking { at, wish } => self.settings_picker_key(&key, *at, wish),
            Mode::Browse if ui.list_focused(narrow) => self.settings_list_key(&ui, key),
            Mode::Browse => self.settings_browse_key(&ui, key, narrow),
        }
    }

    fn settings_put_mode(&mut self, mode: Mode) -> Option<Action> {
        if let Some(ui) = self.settings.as_mut() {
            ui.mode = mode;
        }
        None
    }

    fn settings_row_of(&self, ui: &Settings, key: &str) -> Option<Row> {
        self.settings_view(ui, 0)
            .rows
            .into_iter()
            .find(|r| r.id() == key)
    }

    /// 焦点在段的目录上：↑ ↓ 换段，→ 或 ⏎ 进页。
    fn settings_list_key(&mut self, ui: &Settings, key: Pressed) -> Option<Action> {
        match key {
            Pressed::Up | Pressed::Down => {
                let by = if key == Pressed::Up { -1 } else { 1 };
                self.settings_section(wrapped(ui.section, by, SECTIONS.len()))
            }
            Pressed::Right | Pressed::Enter => {
                if let Some(u) = self.settings.as_mut() {
                    u.in_list = false;
                }
                None
            }
            Pressed::Esc => {
                self.settings_close();
                None
            }
            Pressed::Left | Pressed::Space => None,
            _ => self.settings_browse_key(ui, key, false),
        }
    }

    fn settings_browse_key(&mut self, ui: &Settings, key: Pressed, narrow: bool) -> Option<Action> {
        let rows: Vec<Row> = self
            .settings_view(ui, 0)
            .rows
            .into_iter()
            .filter(Row::selectable)
            .collect();
        let current = self.settings_chosen(ui, 0);
        let at = current.as_ref().map_or(0, |(i, _)| *i);
        let page = ui.page();
        let choose = |app: &mut App, by: i64| {
            if rows.is_empty() {
                return None;
            }
            let j = wrapped(at, by, rows.len());
            let id = (!ui.searching()).then(|| rows[j].id());
            if let Some(u) = app.settings.as_mut() {
                u.test = None;
                match id {
                    Some(id) => {
                        u.chosen.insert(page.clone(), id);
                    }
                    None => u.search_chosen = j,
                }
            }
            None
        };
        let searched = |app: &mut App, d: Field| {
            let first = !d.text.is_empty() && !ui.searching();
            if let Some(u) = app.settings.as_mut() {
                u.search = d;
                u.search_chosen = 0;
            }
            if first {
                app.settings.as_ref().and_then(super::read_all)
            } else {
                None
            }
        };
        let clear = |app: &mut App| {
            if let Some(u) = app.settings.as_mut() {
                u.search = Field::default();
                u.search_chosen = 0;
            }
            None
        };
        match key {
            Pressed::Up => choose(self, -1),
            Pressed::Down => choose(self, 1),
            Pressed::Left if !narrow && !ui.searching() => {
                if let Some(u) = self.settings.as_mut() {
                    u.in_list = true;
                    u.test = None;
                }
                None
            }
            Pressed::Left | Pressed::Right if narrow && ui.stack.is_empty() && !ui.searching() => {
                let by = if key == Pressed::Right { 1 } else { -1 };
                self.settings_section(wrapped(ui.section, by, SECTIONS.len()))
            }
            Pressed::Esc if ui.searching() => clear(self),
            Pressed::Esc if !ui.stack.is_empty() => {
                if let Some(u) = self.settings.as_mut() {
                    u.stack.pop();
                    u.said = None;
                    u.test = None;
                }
                None
            }
            Pressed::Esc => {
                self.settings_close();
                None
            }
            Pressed::CtrlC if ui.searching() => clear(self),
            Pressed::CtrlC => {
                self.settings_close();
                None
            }
            Pressed::Enter if ui.searching() => current.and_then(|(i, _)| self.settings_go_to(i)),
            Pressed::Enter => current.and_then(|(_, r)| self.settings_enter(r)),
            Pressed::Space if !ui.searching() => current.and_then(|(_, r)| self.settings_space(r)),
            // ⌫ 在能删的权限规则上删它（先问）；搜索时照旧删搜索的字。
            Pressed::Backspace if !ui.searching() => match current {
                Some((_, Row::Item(it))) => match *it {
                    ItemRow {
                        key,
                        remove: Some(act),
                        ..
                    } => self.settings_run(&key, act),
                    _ => None,
                },
                _ => None,
            },
            Pressed::Backspace => searched(self, ui.search.backspace()),
            Pressed::CtrlU => searched(self, Field::default()),
            Pressed::Space => searched(self, ui.search.insert(" ")),
            Pressed::Text(t) => searched(self, ui.search.insert(&t)),
            _ => None,
        }
    }

    /// 行上的 ⏎：改它或打开它。
    fn settings_enter(&mut self, row: Row) -> Option<Action> {
        match row {
            Row::Switch { key, on, set, .. } => self.settings_run(&key, set(!on)),
            Row::Choice {
                key,
                options,
                current,
                ..
            } => {
                let at = options.iter().position(|o| o.value == current).unwrap_or(0);
                self.settings_put_mode(Mode::Choosing { key, at })
            }
            Row::Text(t) => {
                let start = if t.secret { String::new() } else { t.value };
                self.settings_put_mode(Mode::Editing {
                    key: t.key,
                    draft: Field::at_end(start),
                })
            }
            Row::Picker { value, .. } => {
                let models = self.settings_models();
                let at = value.as_ref().map_or(0, |p| {
                    models
                        .iter()
                        .position(|m| m.provider == p.provider && m.model == p.model)
                        .unwrap_or(0)
                });
                let wish = value.map(|p| p.effort).unwrap_or_default();
                self.settings_put_mode(Mode::Picking { at, wish })
            }
            Row::Editor { key, path, .. } => self.settings_run(&key, Act::Edit(path)),
            Row::Item(it) => match it.open {
                Some(act) => self.settings_run(&it.key, act),
                None => None,
            },
            Row::Action { key, act, .. } => self.settings_run(&key, act),
            _ => None,
        }
    }

    /// 空格：开关一项。
    fn settings_space(&mut self, row: Row) -> Option<Action> {
        match row {
            Row::Switch { key, on, set, .. } => self.settings_run(&key, set(!on)),
            Row::Item(it) => match it.flip {
                Some(act) => self.settings_run(&it.key, act),
                None => None,
            },
            _ => None,
        }
    }

    /// 缺省模型的选择器列的模型（读 Models 段时一起读到的模型目录）。
    pub(super) fn settings_models(&self) -> Vec<CatalogModel> {
        match self
            .settings
            .as_ref()
            .and_then(|ui| ui.data.get(&super::data::Section::Models))
        {
            Some(super::data::SectionData::Models(m)) => m.catalog.clone(),
            _ => Vec::new(),
        }
    }

    fn settings_edit_key(&mut self, key: &Pressed, k: &str, d: &Field) -> Option<Action> {
        let put = |app: &mut App, next: Field| {
            app.settings_put_mode(Mode::Editing {
                key: k.to_owned(),
                draft: next,
            })
        };
        match key {
            Pressed::Esc => self.settings_put_mode(Mode::Browse),
            Pressed::Enter => {
                let ui = self.settings.clone()?;
                match self.settings_row_of(&ui, k) {
                    // 草稿有错时 ⏎ 不存，留在编辑里。
                    Some(Row::Text(t)) if t.stuck => None,
                    Some(Row::Text(t)) => {
                        let v = d.text.clone();
                        if !t.secret && v == t.value {
                            self.settings_put_mode(Mode::Browse)
                        } else {
                            self.settings_run(&t.key, (t.save)(&v))
                        }
                    }
                    _ => self.settings_put_mode(Mode::Browse),
                }
            }
            Pressed::Text(t) => put(self, d.insert(t)),
            Pressed::Space => put(self, d.insert(" ")),
            Pressed::Backspace => put(self, d.backspace()),
            Pressed::Delete => put(self, d.delete_forward()),
            Pressed::Left => put(self, d.left()),
            Pressed::Right => put(self, d.right()),
            Pressed::Home => put(self, d.to_start()),
            Pressed::End => put(self, d.to_end()),
            Pressed::CtrlU => put(self, d.cut_to_start()),
            Pressed::CtrlK => put(self, d.cut_to_end()),
            Pressed::CtrlW => put(self, d.cut_word_left()),
            Pressed::WordLeft => put(self, d.word_left()),
            Pressed::WordRight => put(self, d.word_right()),
            _ => None,
        }
    }

    fn settings_choosing_key(
        &mut self,
        ui: &Settings,
        key: &Pressed,
        k: &str,
        at: usize,
    ) -> Option<Action> {
        let Some(Row::Choice {
            key: row_key,
            options,
            current,
            pick,
            ..
        }) = self.settings_row_of(ui, k)
        else {
            return self.settings_put_mode(Mode::Browse);
        };
        let n = options.len();
        let choosing = |at| Mode::Choosing {
            key: k.to_owned(),
            at,
        };
        match key {
            Pressed::Up => self.settings_put_mode(choosing(wrapped(at, -1, n))),
            Pressed::Down => self.settings_put_mode(choosing(wrapped(at, 1, n))),
            Pressed::Esc => self.settings_put_mode(Mode::Browse),
            Pressed::Enter if n > 0 => {
                let o = &options[at.min(n - 1)];
                if !o.disabled.is_empty() {
                    None
                } else if o.value == current {
                    self.settings_put_mode(Mode::Browse)
                } else {
                    self.settings_run(&row_key, pick(&o.value))
                }
            }
            _ => None,
        }
    }

    fn settings_confirm_key(&mut self, key: &Pressed) -> Option<Action> {
        let Some(Mode::Confirming {
            key: k,
            at,
            label,
            note,
            act,
        }) = self.settings.as_ref().map(|u| u.mode.clone())
        else {
            return None;
        };
        match key {
            Pressed::Up | Pressed::Down => self.settings_put_mode(Mode::Confirming {
                key: k,
                at: 1 - at,
                label,
                note,
                act,
            }),
            Pressed::Esc => self.settings_put_mode(Mode::Browse),
            Pressed::Enter if at == 0 => self.settings_run(&k, *act),
            Pressed::Enter => self.settings_put_mode(Mode::Browse),
            _ => None,
        }
    }

    fn settings_picker_key(&mut self, key: &Pressed, at: usize, wish: &str) -> Option<Action> {
        let models = self.settings_models();
        let n = models.len();
        let picking = |at, wish: String| Mode::Picking { at, wish };
        match key {
            Pressed::Esc => self.settings_put_mode(Mode::Browse),
            Pressed::Up if n > 0 => {
                self.settings_put_mode(picking(wrapped(at, -1, n), wish.to_owned()))
            }
            Pressed::Down if n > 0 => {
                self.settings_put_mode(picking(wrapped(at, 1, n), wish.to_owned()))
            }
            Pressed::Left | Pressed::Right if n > 0 => {
                let m = &models[at.min(n - 1)];
                let now = effort_of(m, wish);
                let i = m
                    .reasoning_levels
                    .iter()
                    .position(|e| *e == now)
                    .map_or(-1, |i| i as i64)
                    + if *key == Pressed::Right { 1 } else { -1 };
                if i < 0 || i as usize >= m.reasoning_levels.len() {
                    None
                } else {
                    let next = m.reasoning_levels[i as usize].clone();
                    self.settings_put_mode(picking(at, next))
                }
            }
            Pressed::Enter if n > 0 => {
                let m = &models[at.min(n - 1)];
                let effort = effort_of(m, wish);
                let pick = super::data::Pick {
                    provider: m.provider.clone(),
                    model: m.model.clone(),
                    effort: effort.clone(),
                };
                let tail = if effort.is_empty() {
                    String::new()
                } else {
                    format!(" · {effort}")
                };
                let said = format!("Saved. New sessions start on {}{tail}.", m.model);
                let write = Write::new(Change::DefaultModel { pick }, Applies::Next).saying(said);
                self.settings_run("defaultModel", Act::Write(write))
            }
            _ => None,
        }
    }
}
