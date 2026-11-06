//! `$` 面板在 App 上（tui-input.md §1.4；Puffin `shelf.rs` 的 `mention_open`、`mention_key`、`settle_mention`，
//! `mentions.rs` 的 `rows`、`keys`、`paint`）：按键、这一帧的淡入、面板的行、槽，与草稿里认识的提及的墨色。
use crossterm::event::{KeyCode, KeyEvent};
use rust_cli_tui_kit::app::element::Element;
use rust_cli_tui_kit::app::widgets::fade;
use rust_cli_tui_kit::app::widgets::key_hints;
use rust_cli_tui_kit::app::widgets::skill_panel::{self as kit, Panel, SKILL_WORDS, SkillWords};
use rust_cli_tui_kit::render::{Span, Style};

use super::app::{Action, App};
use super::composer::Popup;
use super::i18n::Lang;
use super::skill_mention;

/// 面板里的字：英文是 Puffin 的（kit `SKILL_WORDS`），中文是译文。
fn words(lang: Lang) -> SkillWords {
    match lang {
        Lang::En => SKILL_WORDS,
        Lang::Zh => SkillWords {
            plugin: |p| format!("插件 {p}"),
            this_project: "这个项目",
            yours: "你的",
            project: "项目",
            user: "你的",
            looking: "正在找技能…",
            none_on: ["没有开启的技能。", "/settings skills", " 可以添加。"],
            no_match: "没有相符的技能。它照文字发出。",
            above: |n| format!("↑ 还有 {n} 个"),
            below: |n| format!("↓ 还有 {n} 个"),
        },
    }
}

impl App {
    fn skill_open(&self) -> bool {
        matches!(self.composer.popup, Some(Popup::Skills { .. }))
    }

    fn skill_choice(&self) -> usize {
        match self.composer.popup {
            Some(Popup::Skills { selected }) => selected,
            _ => 0,
        }
    }

    /// 打开的面板预览的技能。
    fn skill_previewed(&self) -> Option<String> {
        if !self.skill_open() {
            return None;
        }
        let ms = self.composer.skill_matches();
        let last = ms.len().checked_sub(1)?;
        ms.get(self.skill_choice().min(last))
            .map(|k| k.name.clone())
    }

    /// 打开的 `$` 面板拿的键（Puffin `mention_key`）；`None` 让键去编辑草稿。
    pub(super) fn skill_key(&mut self, key: KeyEvent) -> Option<Option<Action>> {
        let ctrl = crate::ui::keyboard::ctrl(&key);
        let ms: Vec<String> = self
            .composer
            .skill_matches()
            .iter()
            .map(|k| k.name.clone())
            .collect();
        let n = ms.len();
        let chosen = self.skill_choice().min(n.saturating_sub(1));
        let set = |app: &mut Self, i: usize| {
            app.composer.popup = Some(Popup::Skills { selected: i });
        };
        match key.code {
            KeyCode::Esc => {
                let (at, _) = self.composer.skill_token()?;
                self.composer.mentions.closed_at = Some(at);
                self.composer.popup = None;
            }
            KeyCode::Up if n > 0 => set(self, (chosen + n - 1) % n),
            KeyCode::Char('p') if ctrl && n > 0 => set(self, (chosen + n - 1) % n),
            KeyCode::Down if n > 0 => set(self, (chosen + 1) % n),
            KeyCode::Char('n') if ctrl && n > 0 => set(self, (chosen + 1) % n),
            KeyCode::Tab | KeyCode::Enter if n > 0 => self.composer.insert_skill(&ms[chosen]),
            _ => return None,
        }
        Some(None)
    }

    /// 画之前（`pane.rs`）：面板打开、预览换了技能、插入了提及，各从这一帧的 tick 淡入（Puffin `settle_mention`）。
    pub fn note_skills(&mut self, tick: i64) {
        // 整块的提及跟上这一帧的文字（`textarea_marks.rs`），草稿的墨色按它画。
        self.composer.area.sync_marks();
        let open = self.skill_open();
        let shown = self.skill_previewed();
        let m = &mut self.composer.mentions;
        if open && !m.seen.0 {
            m.opened_at = tick;
        }
        if shown.is_some() && shown != m.seen.1 {
            m.shown_at = tick;
        }
        if std::mem::take(&mut m.just_inserted) {
            m.inserted_at = tick;
        }
        m.seen = (open, shown);
    }

    /// 面板的行（kit `skill_panel::rows_said`）。
    pub fn skill_lines(&self, width: usize, tick: i64) -> Vec<Element> {
        let m = &self.composer.mentions;
        let found: Vec<kit::Skill<'_>> = self
            .composer
            .skill_matches()
            .into_iter()
            .map(|k| k.kit())
            .collect();
        let panel = Panel {
            catalog: m.catalog.as_ref().map(Vec::len),
            matches: &found,
            choice: self.skill_choice(),
            opened_age: tick - m.opened_at,
            shown_age: tick - m.shown_at,
        };
        kit::rows_said(&self.ink.theme, &panel, width, &words(self.lang))
    }

    /// 槽：kit `keys` 里放得下的（Puffin 的 `keys(mentions::keys(..))`），中文是译文。
    pub fn skill_slot(&self) -> Vec<(String, String)> {
        let m = &self.composer.mentions;
        let pairs = kit::keys(
            m.catalog.as_ref().map(Vec::len),
            self.composer.skill_matches().len(),
        );
        key_hints::pick(&pairs)
            .into_iter()
            .map(|(k, v)| (k, super::at_view::slot_word(self.lang, &v)))
            .collect()
    }

    /// 草稿里认识的 `$名字` 画成技能的墨色；刚插入的那个从板上淡入（Puffin `composer` 的 `ranges` 与 `paint`）。
    pub(super) fn skill_inks(&self, text: &str, tick: i64) -> Vec<(usize, usize, Style)> {
        let m = &self.composer.mentions;
        let Some(catalog) = &m.catalog else {
            return Vec::new();
        };
        let known = |n: &str| catalog.iter().any(|k| k.name == n);
        let theme = &self.ink.theme;
        let age = tick - m.inserted_at;
        let (ia, ib) = m.inserted;
        skill_mention::ranges(text, &known)
            .into_iter()
            .map(|(a, b)| {
                let mut style = Style::fg(theme.skill);
                if age < fade::TICKS && a >= ia && b <= ib {
                    let faded = kit::faded(theme, vec![Span::new("", style)], age);
                    style = faded.first().map_or(style, |s| s.style);
                }
                (a, b, style)
            })
            .collect()
    }

    /// 草稿里整块的提及（`@` 面板插入的会话）画成它那一类的墨色（kit `ink_of_kind`），刚插入的从板上淡入（Puffin
    /// `composer` 的 `refs` 与 `paint`）。
    pub(super) fn mark_inks(&self, tick: i64) -> Vec<(usize, usize, Style)> {
        let m = &self.composer.mentions;
        let theme = &self.ink.theme;
        let age = tick - m.inserted_at;
        let (ia, ib) = m.inserted;
        self.composer
            .area
            .marks()
            .iter()
            .map(|r| {
                let mut style = Style::fg(
                    rust_cli_tui_kit::app::widgets::at_panel::rows::ink_of_kind(theme, r.kind),
                );
                if age < fade::TICKS && r.start >= ia && r.end <= ib {
                    let faded = kit::faded(theme, vec![Span::new("", style)], age);
                    style = faded.first().map_or(style, |s| s.style);
                }
                (r.start, r.end, style)
            })
            .collect()
    }

    /// 刚插入的提及（`$` 或 `@` 会话）的范围里没有墨色的部分（插入时加的空格）也从板上淡入，同 Puffin `paint`：它淡的是
    /// 插入的整个范围。`inks` 是已有的墨色范围（不重叠）。
    pub(super) fn inserted_gaps(
        &self,
        inks: &[(usize, usize, Style)],
        tick: i64,
    ) -> Vec<(usize, usize, Style)> {
        let m = &self.composer.mentions;
        let age = tick - m.inserted_at;
        let (ia, ib) = m.inserted;
        let text = self.composer.area.text();
        if age >= fade::TICKS || ia >= ib || ib > text.len() {
            return Vec::new();
        }
        let style = kit::faded(&self.ink.theme, vec![Span::new("", Style::DEFAULT)], age)
            .first()
            .map_or(Style::DEFAULT, |s| s.style);
        let mut covered: Vec<(usize, usize)> = inks
            .iter()
            .map(|(a, b, _)| (*a, *b))
            .filter(|(a, b)| *b > ia && *a < ib)
            .collect();
        covered.sort_unstable();
        let mut out = Vec::new();
        let mut at = ia;
        for (a, b) in covered {
            if a > at {
                out.push((at, a, style));
            }
            at = at.max(b);
        }
        if at < ib {
            out.push((at, ib, style));
        }
        out.retain(|(a, b, _)| text.is_char_boundary(*a) && text.is_char_boundary(*b));
        out
    }

    /// 面板或插入的提及还在淡入：按动画节拍重画（Puffin `mentions::moving`）。
    pub fn skills_moving(&self, tick: i64) -> bool {
        let m = &self.composer.mentions;
        tick - m.opened_at.max(m.shown_at).max(m.inserted_at) < fade::TICKS
    }
}

#[cfg(test)]
#[path = "skill_view_tests.rs"]
mod tests;
