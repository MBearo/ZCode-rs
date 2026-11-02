//! 自动更新选择屏的状态与排版（docs/specs/rust-cli/update.md §6）。
//!
//! 排版用本工程的组件（tui.md §7.4）：顶上整行 `─` 与粗体标题用强调色，正文缩进两格，选项由选择列表组件画（`› ` 加
//! 底色条）并带序号，最后一行是按键提示组件。状态是要显示的内容与焦点；选择交给调用方，独立进入终端的循环在
//! `standalone_dialog`（与信任对话框共用，F138）。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::i18n::Lang;
use super::i18n_update as text;
use super::keyboard;
use super::palette::Palette;
use super::standalone_dialog::{INDENT, Standalone, hint_line};
use super::text::wrap;
use crate::update::{UpdateChoice, UpdateInfo};

const CHOICES: [UpdateChoice; 3] = [
    UpdateChoice::UpdateNow,
    UpdateChoice::Skip,
    UpdateChoice::SkipUntilNext,
];

/// 选择屏：要显示的内容与焦点（开始在 Update now）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    info: UpdateInfo,
    focus: usize,
}

impl Screen {
    pub fn new(info: UpdateInfo) -> Self {
        Self { info, focus: 0 }
    }

    /// 焦点所在的项。
    fn focused(&self) -> UpdateChoice {
        CHOICES[self.focus]
    }
}

impl Standalone for Screen {
    type Answer = UpdateChoice;
    const NAME: &'static str = "the update prompt";
    /// 输入结束（终端关掉）：照常进入。
    const CLOSED: UpdateChoice = UpdateChoice::Skip;

    /// 一个按键（松开的事件由调用方先滤掉）；选出一项时返回它（按键见 update.md §6.2）。
    fn key(&mut self, key: KeyEvent, _now: Instant) -> Option<UpdateChoice> {
        if keyboard::ctrl(&key) && matches!(key.code, KeyCode::Char('c' | 'd')) {
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

    /// 选择屏没有计时。
    fn redraw_at(&self) -> Option<Instant> {
        None
    }

    fn expire(&mut self, _now: Instant) {}

    /// 画面的行（宽度 `width`）。
    fn lines(&self, palette: &Palette, lang: Lang, width: usize) -> Vec<Line<'static>> {
        let info = &self.info;
        let indent = [Span::raw(INDENT)];
        let secondary = palette.secondary();
        let mut lines = vec![super::style::rule(
            width,
            Style::default().fg(palette.accent),
        )];
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
        lines.push(hint_line(text::keys_hint(lang), palette, width));
        lines
    }
}
