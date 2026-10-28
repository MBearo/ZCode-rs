// 复制自 apps/zcode-host/crates/tui/src/ui/terminal.rs（feat/rs 79b6691c55）。
//! 终端的进入、恢复与临时交还（docs/specs/rust-tui.md §8.1）：常驻 alternate screen、raw mode、
//! 鼠标、括号粘贴、焦点事件；能区分 shift+enter 的终端打开键盘增强。退出、出错、导致退出的 panic 都恢复原样；
//! 被捕获的 panic 不碰终端（修复线 F109，docs/specs/rust-cli/tui.md §7.9）；外部编辑器运行期间把终端临时交还。
use std::io::{Stdout, Write, stdout};
use std::sync::Once;
use std::sync::atomic::{AtomicU8, Ordering};
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

/// 有没有 [`Guard`]：0 没有，1 有，2 有且推过键盘增强。panic hook 只读它（tui.md §7.9）。
static ACTIVE: AtomicU8 = AtomicU8::new(0);

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
    install_panic_log();
    ACTIVE.store(1 + u8::from(enhanced), Ordering::Release);
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

/// 修复线 F109（tui.md §7.9）：以前 hook 对任何 panic 都先恢复终端再写 stderr，可工具任务的 panic 被 base 捕获、
/// 进程继续，TUI 就在普通模式的终端上接着画（屏幕错乱、按键要回车）。现在 hook 只记日志，TUI 运行期间把文字记在
/// 本线程，不碰终端、不写 stderr；导致退出的 panic 由 `Guard::drop` 恢复终端后写出。hook 里不拿会阻塞的锁。
/// 只装一次（信任对话框、更新屏、会话界面都调 `enter`）。
fn install_panic_log() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let at = info.location().map(ToString::to_string).unwrap_or_default();
            let message = info.payload_as_str().unwrap_or("Box<dyn Any>");
            let head = rust_cli_base::log_text::head(message);
            tracing::error!(event = "process.panic", location = %at, error = %head, "a thread panicked");
            let active = ACTIVE.load(Ordering::Acquire);
            if active == 0 {
                return previous(info);
            }
            let backtrace = std::backtrace::Backtrace::capture();
            let trace = match backtrace.status() {
                std::backtrace::BacktraceStatus::Captured => format!("stack backtrace:\n{backtrace}\n"),
                _ => String::new(),
            };
            // 本线程已经记着一条：它还在展开，这次是展开中又 panic，std 随后 abort、等不到 `Guard`，照以前恢复再写。
            if let Some(first) = rust_cli_base::note_panic(format!("panicked at {at}:\n{message}\n{trace}")) {
                ACTIVE.store(0, Ordering::Release);
                restore(active == 2);
                let _ = std::io::stderr().write_all(first.as_bytes());
                previous(info);
            }
        }));
    });
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
        ACTIVE.store(0, Ordering::Release);
        restore(self.enhanced);
        // 导致退出的 panic 展开到这里：终端已经恢复，把 hook 记下的文字写到 stderr（tui.md §7.9）。
        if std::thread::panicking()
            && let Some(text) = rust_cli_base::take_panic_note()
        {
            let _ = std::io::stderr().write_all(text.as_bytes());
        }
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
