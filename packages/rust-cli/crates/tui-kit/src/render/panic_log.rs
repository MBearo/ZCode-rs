//! panic 与终端（照旧 tui 的修复线 F109，docs/specs/rust-cli/tui.md §7.9）。
//!
//! 缺陷修复（2026-10-30）：以前 Puffin 不装 panic hook。工具任务的 panic 由 base 的任务捕获、进程继续，可 Rust 缺省的
//! hook 已经把 `thread '…' panicked at …` 写到 stderr，也就是写在备用屏上；渲染器只重画变了的格子，这段字一直盖在
//! 界面上。现在 hook 只记日志：有 [`RealTerminal`](super::real::RealTerminal) 在原始模式里时，panic 的文字记在本线程
//! （base 的 `note_panic`），不碰终端、不写 stderr；被捕获时捕获点清掉它，展开到 `RealTerminal` 时由它的 `Drop` 在
//! 还原终端之后写出。hook 里不拿会阻塞的锁：状态只是一个原子数。
use std::io::Write;
use std::sync::Once;
use std::sync::atomic::{AtomicU8, Ordering};

use super::ansi;

/// 有没有 `RealTerminal` 在原始模式里：0 没有，1 有，2 有且推过 kitty 键盘增强。hook 只读它。
static RAW: AtomicU8 = AtomicU8::new(0);

/// 入口给的日志：panic 的位置（`文件:行:列`）与消息。
pub type Log = fn(location: &str, message: &str);

/// 进了原始模式（`enter_raw` 成功之后）；`pushed` 是推过 kitty 键盘增强。
pub(super) fn entered(pushed: bool) {
    RAW.store(1 + u8::from(pushed), Ordering::Release);
}

/// 终端还回去了（`close`）。交出终端（编辑器）期间不算离开：那时的 panic 照样不该写到别的程序的屏幕上。
pub(super) fn left() {
    RAW.store(0, Ordering::Release);
}

/// 装 hook（只装一次；信任页、更新屏与会话界面都会调）。`log` 每次 panic 都调；没有终端在原始模式里时再交给之前的 hook。
pub fn install(log: Log) {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let at = info.location().map(ToString::to_string).unwrap_or_default();
            let message = info.payload_as_str().unwrap_or("Box<dyn Any>");
            log(&at, message);
            let raw = RAW.load(Ordering::Acquire);
            if raw == 0 {
                return previous(info);
            }
            let backtrace = std::backtrace::Backtrace::capture();
            let trace = match backtrace.status() {
                std::backtrace::BacktraceStatus::Captured => {
                    format!("stack backtrace:\n{backtrace}\n")
                }
                _ => String::new(),
            };
            // 本线程已经记着一条：它还在展开，这次是展开中又 panic，std 随后 abort、等不到 `Drop`，在这里还原再写。
            if let Some(first) =
                rust_cli_base::note_panic(format!("panicked at {at}:\n{message}\n{trace}"))
            {
                restore(raw == 2);
                let _ = std::io::stderr().write_all(first.as_bytes());
                previous(info);
            }
        }));
    });
}

/// 展开到 `RealTerminal` 的 `Drop` 时：取走本线程记下的文字（还原终端之后写到 stderr）。
pub(super) fn take_note() -> Option<String> {
    rust_cli_base::take_panic_note()
}

/// 展开中又 panic 时不经 `RealTerminal` 还原终端（它等不到 `Drop`）：固定的序列，没开过的模式写关也无害。不还原涂过的
/// 终端颜色——只有渲染器知道涂过没有。
fn restore(pushed: bool) {
    RAW.store(0, Ordering::Release);
    let mut s = String::from(ansi::WRAP_ON);
    // 备用屏那一份 kitty 键盘增强在离开之前 pop，主屏那一份在之后（每块缓冲一对，见 `RealTerminal::alt_screen_teardown`）。
    if pushed {
        s.push_str(ansi::POP_KEYBOARD_ENHANCEMENT);
    }
    if cfg!(unix) {
        s.push_str(ansi::MOUSE_DISABLE);
    }
    s.push_str(ansi::ALT_SCREEN_EXIT);
    if cfg!(unix) {
        s.push_str(ansi::BRACKETED_PASTE_DISABLE);
        s.push_str(ansi::FOCUS_DISABLE);
        s.push_str(ansi::APPEARANCE_REPORTS_OFF);
    }
    if pushed {
        s.push_str(ansi::POP_KEYBOARD_ENHANCEMENT);
    }
    s.push_str(ansi::SHOW_CURSOR);
    let mut out = std::io::stdout();
    let _ = out.write_all(s.as_bytes());
    let _ = out.flush();
    // windows 的鼠标是控制台模式，不是 VT 开关（同 `real.rs` 的 `mouse`）。
    #[cfg(windows)]
    let _ = crossterm::execute!(out, crossterm::event::DisableMouseCapture);
    let _ = crossterm::terminal::disable_raw_mode();
}
