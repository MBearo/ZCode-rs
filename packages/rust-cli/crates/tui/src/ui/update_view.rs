//! 自动更新选择屏的状态与排版（docs/specs/rust-cli/update.md §6）。
//!
//! 排版用本工程的组件（tui.md §7.4）：顶上整行 `─` 与粗体标题用强调色，正文缩进两格，选项由选择列表组件画（`› ` 加
//! 底色条）并带序号，最后一行是按键提示组件。只有界面状态（焦点）；选择交给调用方。
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::i18n::Lang;
use super::i18n_update as text;
use super::palette::Palette;
use super::text::{truncate_line, wrap};
use crate::update::{UpdateChoice, UpdateInfo};

const INDENT: &str = "  ";
const CHOICES: [UpdateChoice; 3] = [
    UpdateChoice::UpdateNow,
    UpdateChoice::Skip,
    UpdateChoice::SkipUntilNext,
];

/// 选择屏的界面状态：焦点（开始在 Update now）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Screen {
    focus: usize,
}

impl Screen {
    /// 焦点所在的项。
    pub fn focused(&self) -> UpdateChoice {
        CHOICES[self.focus]
    }

    /// 一个按键（松开的事件由调用方先滤掉）；选出一项时返回它（按键见 update.md §6.2）。
    pub fn key(&mut self, key: KeyEvent) -> Option<UpdateChoice> {
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | 'd'))
        {
            return Some(UpdateChoice::Skip);
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.focus = (self.focus + CHOICES.len() - 1) % CHOICES.len();
            }
            KeyCode::Down | KeyCode::Char('j') => self.focus = (self.focus + 1) % CHOICES.len(),
            KeyCode::Char('1') => return Some(UpdateChoice::UpdateNow),
            KeyCode::Char('2') => return Some(UpdateChoice::Skip),
            KeyCode::Char('3') => return Some(UpdateChoice::SkipUntilNext),
            KeyCode::Enter => return Some(self.focused()),
            KeyCode::Esc => return Some(UpdateChoice::Skip),
            _ => {}
        }
        None
    }

    /// 画面的行（宽度 `width`）。
    pub fn lines(
        &self,
        info: &UpdateInfo,
        palette: &Palette,
        lang: Lang,
        width: usize,
    ) -> Vec<Line<'static>> {
        let indent = [Span::raw(INDENT)];
        let secondary = palette.secondary();
        let mut lines = vec![Line::from(Span::styled(
            "─".repeat(width),
            Style::default().fg(palette.accent),
        ))];
        lines.extend(wrap(
            &[
                Span::styled(
                    text::title(lang),
                    Style::default()
                        .fg(palette.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" · ", secondary),
                Span::styled(format!("{} → {}", info.current, info.latest), secondary),
            ],
            width,
            &indent,
            &indent,
        ));
        lines.extend(wrap(
            &[
                Span::styled(text::release_notes(lang), secondary),
                Span::styled(
                    info.notes_url.clone(),
                    secondary.add_modifier(Modifier::UNDERLINED),
                ),
            ],
            width,
            &indent,
            &indent,
        ));
        lines.push(Line::default());
        for (index, label) in text::options(lang, &info.command).into_iter().enumerate() {
            let chosen = index == self.focus;
            let number = format!("{}. ", index + 1);
            let first = [
                Span::raw(INDENT),
                super::pick::pointer(chosen),
                Span::raw(number.clone()),
            ];
            let rest = [Span::raw(format!(
                "{INDENT}{}{}",
                super::pick::BLANK,
                " ".repeat(number.len())
            ))];
            for line in wrap(&[Span::raw(label)], width, &first, &rest) {
                lines.push(super::pick::paint(line, chosen, palette, width));
            }
        }
        lines.push(Line::default());
        lines.push(truncate_line(
            super::key_hint::line(INDENT.len(), text::keys_hint(lang), palette),
            width,
        ));
        lines
    }
}
