//! 页的复制（docs/specs/rust-cli/tui-kit.md §4.3「页的复制」）：拖选照 Puffin 只在 transcript 里，页的字由页自己的一个键复制，
//! 同 Puffin 门道登录这一步的 `c copy link`。代替正文的页（`page_frame.rs`）按 `c` 复制整页；设置页打的字是它的搜索，按 ⌃y
//! 复制这一屏的页。走 `/copy` 的同一个入口（`Action::Copy`）。复制之后槽里是 fg2 的 `Copied the page`，到下一个键。键不写在
//! 槽里（Puffin 的页的槽没有它），只在 `?` 表旧界面的 `Page` 一组（`footer::sheet_columns`）。
//!
//! 以前（F25）全屏的页可以拖选复制；2026-10-31 照 Puffin 去掉页里的选区时这个能力一起没了，这里照 Puffin 最近的样子补回。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_tui_kit::app::select_units::copy_text;
use rust_cli_tui_kit::render::{Span, StyledLine};

use super::app::{Action, App};
use super::i18n::Lang;

/// 复制出的页的字：照 Puffin「复制的文字」（kit `copy_text`：软折行接回上一行，去掉行尾空白、只有半块边的行与共同的缩进），
/// 再去掉首尾的空行（页的第一行是空行）。
pub fn text_of(lines: &[StyledLine]) -> String {
    copy_text(lines, 0, usize::MAX - 1)
        .trim_matches('\n')
        .to_owned()
}

/// 复制之后槽里的一句（fg2，同 Puffin 门道的 `Copied the link`）。
pub fn said(app: &App) -> Vec<Span> {
    use super::session_page::Copied;
    // Session 标签上 ⏎ 复制的是会话 ID（tui-panels.md §4.5；页的句子是英文，同卡）。
    let text = match (app.page_copied, app.lang) {
        (Some(Copied::SessionId), _) => "Copied the session ID",
        (_, Lang::Zh) => "已复制这一页",
        (_, Lang::En) => "Copied the page",
    };
    vec![Span::new(text, app.ink.theme.fg2)]
}

impl App {
    /// 复制页的键（`keys.rs` 在选区之后、设置页与页自己的键之前问这里）：不是这个键或没有页时为 None。页按上一帧的屏宽重排
    /// （`page_at`），开不开着看现在的状态（`page_frame::body`），不看上一帧画了什么。
    pub(super) fn page_copy_key(&mut self, key: &KeyEvent, now: Instant) -> Option<Option<Action>> {
        let ctrl = crate::ui::keyboard::ctrl(key);
        let alt = crate::ui::keyboard::alt(key);
        let sup = key.modifiers.contains(KeyModifiers::SUPER);
        let lines = if self.settings.is_some() {
            let ctrl_y = ctrl && !alt && !sup && matches!(key.code, KeyCode::Char('y' | 'Y'));
            if !ctrl_y {
                return None;
            }
            self.settings_shown.clone()
        } else {
            if key.code != KeyCode::Char('c') || ctrl || alt || sup {
                return None;
            }
            let (width, now_ms) = self.page_at;
            // 驾驶舱上 `c` 是筛选的字。
            let page =
                super::page_frame::body(self, width, now, now_ms).filter(|page| !page.fit)?;
            page.lines
        };
        let text = text_of(&lines);
        if text.is_empty() {
            return Some(None);
        }
        self.page_copied = Some(super::session_page::Copied::Page);
        Some(Some(Action::Copy(text)))
    }
}

#[cfg(test)]
#[path = "page_copy_tests.rs"]
mod tests;
