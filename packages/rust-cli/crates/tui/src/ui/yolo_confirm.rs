//! 切到完全访问的二次确认（docs/specs/rust-cli/tui.md §7.8，工作线 F87，用户决定 2026-10-24：照 App 的确认框，是局部的
//! 小框）。App 的出处：`packages/ui/src/hooks/useConfirmDialog.ts` 的 `useYoloModeConfirmation` 与
//! `packages/ui/src/ConfirmDialog.tsx` 的 yolo-confirmation 分支。
//!
//! 只有显式入口（shift+tab、`/mode yolo`、`/mode` 选择器）从别的模式切到完全访问时才问；框只是界面状态（`Overlay`），
//! 确认之前不发任何命令，确认后才发 `SetMode`，模式以内核的 `ModeChanged` 为准。框开着时按键只给它，所以不会叠框。
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use rust_cli_kernel_api::permission::PermissionMode;
use unicode_width::UnicodeWidthStr;

use super::app::{Action, App, Overlay};
use super::i18n_yolo as words;
use super::interaction_view::{View, bold, checkbox, dim, hint, named, option};
use super::text::{wrap, wrap_text};

/// 焦点所在的一处：勾选框、取消、确认启用（从上到下）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    Acknowledge,
    /// 打开时焦点在取消（App 打开时把焦点放在取消按钮上）。
    #[default]
    Cancel,
    Confirm,
}

impl Focus {
    const ORDER: [Self; 3] = [Self::Acknowledge, Self::Cancel, Self::Confirm];

    /// ↑ / ↓ 移一处，到头回绕。
    fn step(self, down: bool) -> Self {
        let at = Self::ORDER.iter().position(|f| *f == self).unwrap_or(1);
        let next = if down {
            at + 1
        } else {
            at + Self::ORDER.len() - 1
        };
        Self::ORDER[next % Self::ORDER.len()]
    }
}

/// 确认框的状态。勾选只属于这一次打开（App：关闭后再打开不沿用上次的风险确认）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct YoloConfirm {
    pub checked: bool,
    pub focus: Focus,
    /// 确认后提示 `Mode switched to Full access.`：`/mode` 的两个入口提示，shift+tab 不提示（与以前相同）。
    pub announce: bool,
}

impl App {
    /// 显式入口要切到 `mode`（tui.md §7.8）：从别的模式切到完全访问时弹确认框、不发命令（框已经开着时什么都不做，
    /// 不叠框、不重置）；其余照旧 `SetMode`，`announce` 时先提示。
    pub(super) fn switch_mode(&mut self, mode: PermissionMode, announce: bool) -> Option<Action> {
        let full = PermissionMode::BypassPermissions;
        if mode == full && self.view.mode() != full {
            if !matches!(self.overlay, Some(Overlay::YoloConfirm(_))) {
                self.overlay = Some(Overlay::YoloConfirm(YoloConfirm {
                    announce,
                    ..YoloConfirm::default()
                }));
            }
            return None;
        }
        if announce {
            self.mode_switched(mode);
        }
        Some(Action::SetMode(mode))
    }

    /// 确认框开着时的按键：↑↓ 移动，Space 勾选（焦点在勾选框上时），Enter 作用于当前项，`1` / `2` 作用于对应序号的选项，
    /// Esc 与 ctrl+c 取消；其余不起作用。
    pub(super) fn yolo_confirm_key(&mut self, key: KeyEvent) -> Option<Action> {
        let ctrl = self.typing.ctrl(&key);
        let Some(Overlay::YoloConfirm(confirm)) = self.overlay.as_mut() else {
            return None;
        };
        // 作用于哪一项。数字键照别的询问类对话框（权限询问、提问框，工作线 F92）：等于在那一项上按 Enter，不看焦点。
        let chosen = match key.code {
            KeyCode::Up => {
                confirm.focus = confirm.focus.step(false);
                return None;
            }
            KeyCode::Down => {
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
                let announce = confirm.announce;
                self.overlay = None;
                if announce {
                    self.mode_switched(PermissionMode::BypassPermissions);
                }
                return Some(Action::SetMode(PermissionMode::BypassPermissions));
            }
        }
        None
    }
}

/// 块之间的空行（块是：线、标题与说明；三项；风险段；勾选框与选项；提示）：空行在哪个块前面，要紧的在前。放不下时从后面
/// 去掉：先去风险段前面的，再去标题块后面的、提示前面的，勾选框前面的留到最后。
const GAPS: [usize; 4] = [3, 4, 1, 2];

/// 框的行（tui.md §7.8）：警告色的线与 `⚠` 标题、说明、三项、风险段、勾选框、取消 / 确认启用、按键提示。`height` 是底部
/// 区域能用的行数：连同块之间的空行放不下时按 [`GAPS`] 的先后去掉空行。所有文字按宽度折行。
pub fn lines(confirm: &YoloConfirm, view: &View<'_>, height: usize) -> Vec<Line<'static>> {
    let (lang, width) = (view.lang, view.width);
    let warning = view.palette.warning();
    let indent = || [Span::raw("  ")];
    let mut header = vec![Line::from(Span::styled("─".repeat(width), warning))];
    header.extend(wrap_text(
        &format!("⚠ {}", words::title(lang)),
        warning.add_modifier(Modifier::BOLD),
        width,
        &indent(),
        &[Span::raw("    ")],
    ));
    header.extend(wrap_text(
        words::description(lang),
        Style::default(),
        width,
        &indent(),
        &indent(),
    ));
    let blocks = [
        header,
        items(view),
        wrap_text(
            words::risk(lang),
            view.palette.secondary(),
            width,
            &indent(),
            &indent(),
        ),
        choices(confirm, view),
        vec![hint(words::hint(lang), view)],
    ];
    let total: usize = blocks.iter().map(Vec::len).sum();
    let gaps = &GAPS[..height.saturating_sub(total).min(GAPS.len())];
    let mut lines = Vec::new();
    for (index, block) in blocks.into_iter().enumerate() {
        if gaps.contains(&index) {
            lines.push(Line::default());
        }
        lines.extend(block);
    }
    lines
}

/// 三项：粗体的名称补到最长的名称，两个空格后是次要文字的说明，续行与说明对齐；说明那一列超过宽度一半时说明另起一行、
/// 缩进 4 列。
fn items(view: &View<'_>) -> Vec<Line<'static>> {
    let items = words::items(view.lang);
    let column = items
        .iter()
        .map(|(name, _)| name.width())
        .max()
        .unwrap_or(0);
    let start = 2 + column + 2;
    let secondary = view.palette.secondary();
    let mut lines = Vec::new();
    for (name, description) in items {
        let description = [Span::styled(description, secondary)];
        if start <= view.width / 2 {
            let first = [
                Span::raw("  "),
                Span::styled(name, bold()),
                Span::raw(" ".repeat(column - name.width() + 2)),
            ];
            lines.extend(wrap(
                &description,
                view.width,
                &first,
                &[Span::raw(" ".repeat(start))],
            ));
        } else {
            let at = |n: usize| [Span::raw(" ".repeat(n))];
            lines.extend(wrap_text(name, bold(), view.width, &at(2), &at(2)));
            lines.extend(wrap(&description, view.width, &at(4), &at(4)));
        }
    }
    lines
}

/// 勾选框与两个选项：焦点行是强调色的 `❯`（tui.md §7.7）；勾选框的文字聚焦时是强调色；“确认启用”没勾选时 dim（不可用），
/// 勾选后是 destructive。
fn choices(confirm: &YoloConfirm, view: &View<'_>) -> Vec<Line<'static>> {
    let accent = Style::default().fg(view.palette.accent);
    let focused = |focus| confirm.focus == focus;
    let mut first = vec![
        Span::raw("  "),
        if focused(Focus::Acknowledge) {
            Span::styled("❯ ", accent)
        } else {
            Span::raw("  ")
        },
    ];
    first.extend(checkbox(confirm.checked, view));
    let style = if focused(Focus::Acknowledge) {
        accent
    } else {
        Style::default()
    };
    let text = [Span::styled(words::acknowledgement(view.lang), style)];
    let rest = [Span::raw(" ".repeat(8))];
    let mut lines = wrap(&text, view.width, &first, &rest);
    let cancel = named(words::cancel(view.lang), focused(Focus::Cancel), view);
    lines.push(option(focused(Focus::Cancel), 1, vec![cancel], view));
    let style = if confirm.checked {
        view.palette.destructive()
    } else {
        dim()
    };
    let label = Span::styled(words::confirm(view.lang), style);
    lines.push(option(focused(Focus::Confirm), 2, vec![label], view));
    lines
}

#[cfg(test)]
#[path = "yolo_confirm_tests.rs"]
mod tests;
