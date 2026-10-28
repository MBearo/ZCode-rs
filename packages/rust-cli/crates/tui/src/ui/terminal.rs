// 复制自 apps/zcode-host/crates/tui/src/ui/terminal.rs（feat/rs 79b6691c55）。
//! 终端的进入、恢复与临时交还（docs/specs/rust-tui.md §8.1）：常驻 alternate screen、raw mode、
//! 鼠标、括号粘贴、焦点事件；能区分 shift+enter 的终端打开键盘增强。退出、出错、panic 都恢复原样；
//! 外部编辑器运行期间把终端临时交还。
use std::io::{Stdout, stdout};
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use super::palette;

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// 背景探测最多等这么久（不回应 OSC 11 的终端也会回应 DA1，通常远早于此）。
const PROBE_TIMEOUT: Duration = Duration::from_millis(250);

/// 持有期间终端处于 TUI 状态；释放时恢复。
pub struct Guard {
    enhanced: bool,
}

/// 进入 TUI，返回终端、守卫与探测到的前景色与背景色。
pub fn enter() -> Result<(Tui, Guard, palette::Probed)> {
    enable_raw_mode()?;
    // 在 crossterm 开始读输入之前探测，回复不会被当作按键。
    let background = palette::probe_colors(PROBE_TIMEOUT);
    let enhanced = supports_keyboard_enhancement().unwrap_or(false);
    // 缺陷修复（F65 的 T9）：以前设模式失败时直接返回，终端停在 raw mode 与备用屏（旧版 Windows 控制台不支持 VT 时
    // `EnableBracketedPaste` 会失败）。现在先恢复再返回错误。
    let terminal = undo_on_error(
        || {
            set_modes(enhanced)?;
            Ok(Terminal::new(CrosstermBackend::new(stdout()))?)
        },
        || restore(enhanced),
    )?;
    install_panic_restore(enhanced);
    Ok((terminal, Guard { enhanced }, background))
}

/// 跑 `step`；失败时先跑 `undo` 再返回错误。
fn undo_on_error<T>(step: impl FnOnce() -> Result<T>, undo: impl FnOnce()) -> Result<T> {
    let result = step();
    if result.is_err() {
        undo();
    }
    result
}

fn set_modes(enhanced: bool) -> Result<()> {
    let mut out = stdout();
    execute!(
        out,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste,
        EnableFocusChange
    )?;
    if enhanced {
        execute!(
            out,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }
    Ok(())
}

fn restore(enhanced: bool) {
    let mut out = stdout();
    if enhanced {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(
        out,
        DisableFocusChange,
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen
    );
    let _ = disable_raw_mode();
}

/// panic 时先恢复终端，再交给原来的 hook：否则信息落在 alternate screen 里看不到。
fn install_panic_restore(enhanced: bool) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore(enhanced);
        previous(info);
    }));
}

impl Guard {
    /// 终端分辨得出增强按键吗（进入时键盘增强的查询结论，修复线 F104，tui-input.md §11.1.1）：为真时推过
    /// `DISAMBIGUATE_ESCAPE_CODES`，ctrl+enter 才与回车分得开。
    pub fn enhanced(&self) -> bool {
        self.enhanced
    }

    /// 临时交还终端（外部编辑器）；返回后重新进入并要求整屏重绘。
    pub fn suspend(&self) {
        restore(self.enhanced);
    }

    /// 收回终端（raw mode 与各种模式）；整屏重画由调用方做（tui-input.md §4.1）。
    pub fn reclaim(&self) -> Result<()> {
        enable_raw_mode()?;
        set_modes(self.enhanced)
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        restore(self.enhanced);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::undo_on_error;

    /// T9：进入终端的某一步失败时，先恢复再返回错误；成功时不恢复。
    #[test]
    fn a_failed_step_is_undone_before_the_error_returns() {
        let undone = Cell::new(0);
        let failed: anyhow::Result<()> = undo_on_error(
            || Err(anyhow::anyhow!("no VT")),
            || undone.set(undone.get() + 1),
        );
        assert_eq!(failed.unwrap_err().to_string(), "no VT");
        assert_eq!(undone.get(), 1);
        let entered = undo_on_error(|| Ok(7), || undone.set(undone.get() + 1));
        assert_eq!(entered.unwrap(), 7);
        assert_eq!(undone.get(), 1);
    }
}
