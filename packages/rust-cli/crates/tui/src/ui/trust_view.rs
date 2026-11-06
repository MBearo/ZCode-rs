//! 工作区信任对话框的状态与排版（docs/specs/rust-cli/workspace-trust.md §2.4、§2.5）。
//!
//! 照 Puffin 的信任页（docs/specs/rust-cli/workspace-trust.md，壳的 `kept/trust.rs`；tui-kit.md §4.2「工作区信任」）：行是
//! kit 的 `kept_rows::trust_rows`（中文界面用 `trust_rows_said` 换字），顶上空一行；键照 `trust_key`：↑ ^P 到取消项、↓ ^N
//! 到信任项，⏎ 选焦点的项，`1` / `2` 直接选，esc 未信任时退出、后备询问时不带授权继续；^C 或 ^D 连按两次退出，按了一次
//! 之后最后一行说 `^C again to exit`，不会过时，按别的键才清掉（以前过 [`super::notice::DOUBLE`] 就清掉）。应用内切项目时
//! 的这个对话框与启动前的信任页（`screens::ask_trust`）同一个样子：两处的提示都不到期，所以不用到期重画（F138-2 修的是提示
//! 到期之后还留在屏上；这里显示与行为一致：提示在，再按一次就退出）。状态是要显示的内容与界面状态（焦点、连按的键）；
//! 回答交给调用方。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use rust_cli_tui_kit::app::element::{br, stack};
use rust_cli_tui_kit::app::layout::Layout;
use rust_cli_tui_kit::app::widgets::kept_rows::{self, TrustWords};
use rust_cli_tui_kit::render::StyledLine;

use super::i18n::Lang;
use super::i18n_trust as text;
use super::ink::Ink;
use super::keyboard;
use crate::trust::{TrustAnswer, TrustPrompt};

/// 对话框：要显示的内容、焦点（0 是取消项，缺省焦点在取消项）与连按退出的键（`C` 或 `D`）。
#[derive(Debug, Clone, PartialEq)]
pub struct Dialog {
    prompt: TrustPrompt,
    focus: usize,
    armed: Option<char>,
}

impl Dialog {
    pub fn new(prompt: TrustPrompt) -> Self {
        Self {
            prompt,
            focus: 0,
            armed: None,
        }
    }

    /// 一个按键；给出回答时对话框结束（Puffin `trust_key`）。
    pub fn key(&mut self, key: KeyEvent, _now: Instant) -> Option<TrustAnswer> {
        let ctrl = keyboard::ctrl(&key);
        if ctrl && let KeyCode::Char(c @ ('c' | 'd')) = key.code {
            let c = c.to_ascii_uppercase();
            if self.armed == Some(c) {
                return Some(TrustAnswer::Exit(1));
            }
            self.armed = Some(c);
            return None;
        }
        self.armed = None;
        match key.code {
            KeyCode::Up => self.focus = 0,
            KeyCode::Char('p') if ctrl => self.focus = 0,
            KeyCode::Down => self.focus = 1,
            KeyCode::Char('n') if ctrl => self.focus = 1,
            KeyCode::Enter => return Some(self.chosen(self.focus)),
            KeyCode::Char('1') => return Some(self.chosen(0)),
            KeyCode::Char('2') => return Some(self.chosen(1)),
            // 未信任时 esc 退出（退出码 0）；后备询问时 esc 等于“不带这些权限继续”。
            KeyCode::Esc if self.prompt.backstop => return Some(TrustAnswer::WithoutGrants),
            KeyCode::Esc => return Some(TrustAnswer::Exit(0)),
            _ => {}
        }
        None
    }

    fn chosen(&self, focus: usize) -> TrustAnswer {
        match (focus, self.prompt.backstop) {
            (1, _) => TrustAnswer::Trusted,
            (_, true) => TrustAnswer::WithoutGrants,
            (_, false) => TrustAnswer::Exit(1),
        }
    }

    /// 画面的行（宽度 `width`）：顶上空一行，然后是 kit 的 `trust_rows`。
    pub fn lines(&self, ink: &Ink, lang: Lang, width: usize) -> Vec<StyledLine> {
        let prompt = &self.prompt;
        let grants = prompt.grants.as_ref().map(|g| kept_rows::Grants {
            count: g.count,
            rules: &g.rules,
            sources: &g.sources,
        });
        let trust = kept_rows::Trust {
            path: &prompt.path,
            backstop: prompt.backstop,
            grants,
        };
        let words = match lang {
            Lang::En => kept_rows::trust_words(&trust),
            Lang::Zh => zh_words(prompt, lang),
        };
        let mut rows = vec![br()];
        rows.extend(kept_rows::trust_rows_said(
            &ink.theme, &trust, &words, self.focus, self.armed,
        ));
        Layout::lay(&stack(rows), width)
    }
}

/// 中文界面的字（`i18n_trust.rs`）。
fn zh_words(prompt: &TrustPrompt, lang: Lang) -> TrustWords<'static> {
    let (grants_title, grants_rules) = prompt.grants.as_ref().map_or_else(Default::default, |g| {
        (
            text::grants_title(lang, g.count, &text::list(lang, &g.sources, None)),
            text::list(lang, &g.rules, Some(RULES_SHOWN)),
        )
    });
    TrustWords {
        title: text::title(lang),
        safety_check: text::safety_check(lang),
        abilities: text::abilities(lang),
        grants_title,
        grants_rules,
        grants_note: text::grants_note(lang),
        cancel: text::cancel(lang, prompt.backstop),
        confirm: text::confirm(lang),
        keys: text::keys(lang),
        again: text::again(lang),
    }
}

/// 列出的规则最多几个（同 kit）。
const RULES_SHOWN: usize = 8;
