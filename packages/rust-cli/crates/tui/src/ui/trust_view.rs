//! 工作区信任对话框的状态与排版（docs/specs/rust-cli/workspace-trust.md §2.4、§2.5）。
//!
//! 排版沿用审批视图（`interaction_view`）：整行 `─` 与粗体标题（F26 起用主题的强调色，用户决定），
//! 正文缩进两格、段落之间空一行，焦点行按选择列表组件画（`› ` 加底色条，F26），选项不带序号，最后一行是按键提示组件。
//! 只有界面状态（焦点、连按的提示）；回答交给调用方。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::i18n::Lang;
use super::i18n_trust as text;
use super::notice::Armed;
use super::palette::Palette;
use super::text::{truncate_line, wrap};
use crate::trust::{TrustAnswer, TrustPrompt};

/// 警告色（`/context` 的警告也用它，深浅两种背景都可读）：F26 起只给项目授权那段的 `⚠` 标题。
const WARNING: Color = Color::Yellow;
/// 列出的规则最多几个。
const RULES_SHOWN: usize = 8;
const INDENT: &str = "  ";

/// 对话框的界面状态：焦点（0 是取消项，缺省焦点在取消项）与连按两次退出的提示。
#[derive(Debug, Clone, PartialEq)]
pub struct Dialog {
    backstop: bool,
    focus: usize,
    armed: Option<(Armed, Instant)>,
}

impl Dialog {
    pub fn new(backstop: bool) -> Self {
        Self {
            backstop,
            focus: 0,
            armed: None,
        }
    }

    /// 连按提示的到期时刻（到期后重画，提示行回到按键说明）。
    pub fn hint_until(&self) -> Option<Instant> {
        self.armed.map(|(_, until)| until)
    }

    /// 提示到期：清掉。
    pub fn expire(&mut self, now: Instant) {
        if self.armed.is_some_and(|(_, until)| now >= until) {
            self.armed = None;
        }
    }

    /// 一个按键；给出回答时对话框结束。
    pub fn key(&mut self, key: KeyEvent, now: Instant) -> Option<TrustAnswer> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let exit = match key.code {
            KeyCode::Char('c') if ctrl => Some(Armed::ExitC),
            KeyCode::Char('d') if ctrl => Some(Armed::ExitD),
            _ => None,
        };
        if let Some(exit) = exit {
            // 提示期间再按同一个键退出，退出码 1。
            if self
                .armed
                .is_some_and(|(armed, until)| armed == exit && now < until)
            {
                return Some(TrustAnswer::Exit(1));
            }
            self.armed = Some((exit, now + super::notice::DOUBLE));
            return None;
        }
        match key.code {
            KeyCode::Up => self.focus = 0,
            KeyCode::Down => self.focus = 1,
            KeyCode::Enter => return Some(self.chosen()),
            // 未信任时 esc 退出（退出码 0）；后备询问时 esc 等于“不带这些权限继续”。
            KeyCode::Esc if self.backstop => return Some(TrustAnswer::WithoutGrants),
            KeyCode::Esc => return Some(TrustAnswer::Exit(0)),
            _ => {}
        }
        None
    }

    fn chosen(&self) -> TrustAnswer {
        match (self.focus, self.backstop) {
            (1, _) => TrustAnswer::Trusted,
            (_, true) => TrustAnswer::WithoutGrants,
            (_, false) => TrustAnswer::Exit(1),
        }
    }

    /// 画面的行（宽度 `width`）。
    pub fn lines(
        &self,
        prompt: &TrustPrompt,
        palette: &Palette,
        lang: Lang,
        width: usize,
    ) -> Vec<Line<'static>> {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let dim = Style::default().add_modifier(Modifier::DIM);
        let paragraph = |content: &str, style: Style| {
            wrap(
                &[Span::styled(content.to_owned(), style)],
                width,
                &[Span::raw(INDENT)],
                &[Span::raw(INDENT)],
            )
        };
        // 顶上的线与标题用主题的强调色（修复线 F26，用户决定，workspace-trust.md §2.4；原来是警告色 `WARNING`）。
        let accent = palette.accent;
        let mut lines = vec![
            Line::from(Span::styled("─".repeat(width), Style::default().fg(accent))),
            truncate_line(
                Line::from(vec![
                    Span::raw(INDENT),
                    Span::styled(text::title(lang), bold.fg(accent)),
                ]),
                width,
            ),
            Line::default(),
        ];
        lines.extend(paragraph(&sanitize(&prompt.path), bold));
        lines.push(Line::default());
        lines.extend(paragraph(text::safety_check(lang), Style::default()));
        lines.push(Line::default());
        lines.extend(paragraph(text::abilities(lang), Style::default()));
        if let Some(grants) = &prompt.grants {
            lines.push(Line::default());
            let sources = text::list(lang, &grants.sources, None);
            let title = format!("⚠ {}", text::grants_title(lang, grants.count, &sources));
            lines.extend(paragraph(&title, bold.fg(WARNING)));
            let rules = format!(
                "{INDENT}{}",
                text::list(lang, &grants.rules, Some(RULES_SHOWN))
            );
            lines.extend(paragraph(&rules, Style::default()));
            lines.extend(paragraph(text::grants_note(lang), dim));
        }
        lines.push(Line::default());
        let labels = [text::cancel(lang, self.backstop), text::confirm(lang)];
        for (index, label) in labels.into_iter().enumerate() {
            // 选择列表组件（`› ` 加底色条，tui.md §7.4，修复线 F26；原来是强调色的 `❯ `）。
            let chosen = index == self.focus;
            let line = Line::from(vec![
                Span::raw(INDENT),
                super::pick::pointer(chosen),
                Span::raw(label),
            ]);
            lines.push(super::pick::paint(
                truncate_line(line, width),
                chosen,
                palette,
                width,
            ));
        }
        lines.push(Line::default());
        // 按键提示组件（tui.md §7.4）：连按的提示与按键说明都是 `{键}` 模板。
        let hint = match self.armed {
            Some((armed, _)) => super::notice::armed_text(lang, armed),
            None => text::keys_hint(lang),
        };
        lines.push(truncate_line(
            super::key_hint::line(INDENT.len(), hint, palette),
            width,
        ));
        lines
    }
}

/// 控制字符换成 U+FFFD。
fn sanitize(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_control() { '\u{fffd}' } else { c })
        .collect()
}
