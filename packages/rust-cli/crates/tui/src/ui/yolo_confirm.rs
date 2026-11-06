//! 切到完全访问的二次确认（docs/specs/rust-cli/tui.md §7.8，工作线 F87，用户决定 2026-10-24：照 App 的确认框）。
//! App 的出处：`packages/ui/src/hooks/useConfirmDialog.ts` 的 `useYoloModeConfirmation` 与
//! `packages/ui/src/ConfirmDialog.tsx` 的 yolo-confirmation 分支。
//!
//! 显式入口（⇧⇥、`/mode yolo`、`/mode` 选择器）从别的模式切到完全访问时、权限请求里选 Full access 时问；框只是界面状态
//! （`Overlay`），确认之前不发任何命令。确认后做打开它的那件事：切模式，或允许那个请求这一次再切模式；那个请求离开时框
//! 作废（Puffin `afk_seen`）。
//!
//! 画法照 Puffin（docs/specs/rust-cli/tui.md §7.8，壳的 `kept/yolo.rs`；tui-kit.md §4.2「完全访问的确认」）：
//! 框的行是 kit 的 `kept_rows::yolo_rows`，在板的顶上代替请求的行；输入框与状态栏照常在，槽是 `yolo_slot`。确认后输入框上方
//! 说模式的那一句 `Full access: it runs commands and edits files without asking.`（Puffin 的 `asked`，模式每次变了都说，
//! `status_motion.rs`）。
use crossterm::event::{KeyCode, KeyEvent};
use rust_cli_kernel_api::ids::InteractionId;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_tui_kit::app::widgets::kept_rows::{self, YoloWords};

use super::app::{Action, App, Overlay};
use super::i18n::Lang;
use super::i18n_asks;
use super::i18n_yolo as words;
use super::interaction_view::View;
use super::interaction_view::board::{Laid, lay_all, slot_pairs};
use super::keyboard;

pub use rust_cli_tui_kit::app::widgets::kept_rows::YoloFocus as Focus;

/// 确认框的状态。勾选只属于这一次打开（App：关闭后再打开不沿用上次的风险确认）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct YoloConfirm {
    pub checked: bool,
    pub focus: Focus,
    /// 权限请求里选了 Full access：确认后允许这个请求这一次（选项 id），再切模式（Puffin `YoloThen::Answer`）。
    pub then: Option<(InteractionId, String)>,
}

impl App {
    /// 显式入口要切到 `mode`（tui.md §7.8）：从别的模式切到完全访问时弹确认框、不发命令（框已经开着时什么都不做，
    /// 不叠框、不重置）；其余照旧 `SetMode`，`announce` 时先提示（`/mode` 的入口）。
    pub(super) fn switch_mode(&mut self, mode: PermissionMode, announce: bool) -> Option<Action> {
        let full = PermissionMode::BypassPermissions;
        if mode == full && self.view.mode() != full {
            if !matches!(self.overlay, Some(Overlay::YoloConfirm(_))) {
                self.overlay = Some(Overlay::YoloConfirm(YoloConfirm::default()));
            }
            return None;
        }
        if announce {
            self.mode_switched(mode);
        }
        Some(Action::SetMode(mode))
    }

    /// 权限请求 `ask` 里选了 Full access（选项 `option`）：先问，确认后允许这一次再切模式（Puffin `confirm_yolo` 的
    /// `YoloThen::Answer`）。框已经开着时什么都不做。
    pub(super) fn confirm_yolo_for(&mut self, ask: InteractionId, option: String) {
        if matches!(self.overlay, Some(Overlay::YoloConfirm(_))) {
            return;
        }
        self.overlay = Some(Overlay::YoloConfirm(YoloConfirm {
            then: Some((ask, option)),
            ..YoloConfirm::default()
        }));
    }

    /// 等着回答的请求离开了：框作废（Puffin `afk_seen`）。
    pub(super) fn settle_yolo(&mut self) {
        let Some(Overlay::YoloConfirm(YoloConfirm {
            then: Some((ask, _)),
            ..
        })) = &self.overlay
        else {
            return;
        };
        let open = super::interaction_view::open_asks(self)
            .iter()
            .any(|(id, _)| *id == ask);
        if !open {
            self.overlay = None;
        }
    }

    /// 确认框开着时的按键（Puffin `yolo_key`）：↑↓ ^P ^N 移动，空格勾选（焦点在勾选框上时），⏎ 作用于当前项，`1` 是取消、
    /// `2` 是确认（数字不看焦点，F92），esc 与 ^C 取消；没勾选时确认不起作用。其余的键不起作用。
    pub(super) fn yolo_confirm_key(&mut self, key: KeyEvent) -> Option<Action> {
        let ctrl = keyboard::ctrl(&key);
        let Some(Overlay::YoloConfirm(confirm)) = self.overlay.as_mut() else {
            return None;
        };
        let chosen = match key.code {
            KeyCode::Up => {
                confirm.focus = confirm.focus.step(false);
                return None;
            }
            KeyCode::Char('p') if ctrl => {
                confirm.focus = confirm.focus.step(false);
                return None;
            }
            KeyCode::Down => {
                confirm.focus = confirm.focus.step(true);
                return None;
            }
            KeyCode::Char('n') if ctrl => {
                confirm.focus = confirm.focus.step(true);
                return None;
            }
            KeyCode::Char(' ') if confirm.focus == Focus::Acknowledge => Focus::Acknowledge,
            KeyCode::Enter => confirm.focus,
            KeyCode::Char('1') | KeyCode::Esc => Focus::Cancel,
            KeyCode::Char('c') if ctrl => Focus::Cancel,
            KeyCode::Char('2') => Focus::Confirm,
            _ => return None,
        };
        match chosen {
            Focus::Acknowledge => confirm.checked = !confirm.checked,
            Focus::Cancel => self.overlay = None,
            // 没勾选时确认不可用（Enter 与 `2` 都不起作用，焦点不动）。
            Focus::Confirm if !confirm.checked => {}
            Focus::Confirm => {
                let then = confirm.then.take();
                self.overlay = None;
                return Some(match then {
                    Some((ask, option)) => self.answer_full_access(&ask, &option),
                    None => Action::SetMode(PermissionMode::BypassPermissions),
                });
            }
        }
        None
    }
}

/// 框的字：英文是 kit 的（Puffin 一字不差），中文是 App 的原文（`i18n_yolo.rs`）。
fn yolo_words(lang: Lang) -> YoloWords<'static> {
    match lang {
        Lang::En => kept_rows::YOLO_WORDS,
        Lang::Zh => YoloWords {
            title: words::title(lang),
            description: words::description(lang),
            items: words::items(lang),
            risk: words::risk(lang),
            acknowledgement: words::acknowledgement(lang),
            cancel: words::cancel(lang),
            confirm: words::confirm(lang),
        },
    }
}

/// 槽的键（Puffin `yolo_slot`）：焦点在勾选框上时 `space tick  esc cancel`，否则 `1–2 answer  esc cancel`。
fn slot_words(lang: Lang, focus: Focus) -> Vec<(String, String)> {
    kept_rows::yolo_slot(focus)
        .into_iter()
        .map(|(key, said)| {
            let said = i18n_asks::slot(lang, &said).to_owned();
            (key, said)
        })
        .collect()
}

/// 框的行（Puffin `yolo_rows`）：在板的顶上代替请求的行；没有自己的输入行，草稿行照常（占位照草稿，`answer` 为 None），
/// 槽是框的键。
pub fn lines(confirm: &YoloConfirm, view: &View<'_>, _height: usize) -> Laid {
    let rows = kept_rows::yolo_rows_said(
        view.theme(),
        &yolo_words(view.lang),
        confirm.checked,
        confirm.focus,
    );
    let lines = lay_all(&rows, view.width);
    Laid {
        input_at: lines.len(),
        lines,
        slot: slot_pairs(view.theme(), &slot_words(view.lang, confirm.focus)),
        ..Laid::default()
    }
}

#[cfg(test)]
#[path = "yolo_confirm_tests.rs"]
mod tests;
