//! 进入界面之前对终端的判断（tui-input.md §9.6，工作线 F71）：三种组合与两种语言的文字。

use super::*;
use crate::ui::i18n::{EN, ZH};

#[test]
fn only_a_console_on_both_ends_can_enter() {
    assert_eq!(Console::classify(true, true, true), Console::Ready);
    assert_eq!(Console::classify(false, true, true), Console::NotTerminal);
    assert_eq!(Console::classify(true, false, true), Console::NotTerminal);
    assert_eq!(Console::classify(false, false, false), Console::NotTerminal);
    // mintty：两端都是 msys 的 pty（`is_terminal` 为真），stdin 不是控制台。
    assert_eq!(Console::classify(true, true, false), Console::Mintty);
}

#[test]
fn mintty_is_refused_with_the_way_out() {
    assert_eq!(Console::Ready.refusal(&EN), None);
    assert_eq!(
        Console::NotTerminal.refusal(&EN),
        Some("TUI requires an interactive terminal.")
    );
    assert_eq!(
        Console::Mintty.refusal(&EN),
        Some(
            "This terminal (mintty, the Git Bash window) cannot run ZCode: its keys do not reach Windows programs. Use Windows Terminal, or run `winpty zcode.cmd`."
        )
    );
    assert_eq!(
        Console::Mintty.refusal(&ZH),
        Some(
            "这个终端（mintty，即 Git Bash 的窗口）不能运行 ZCode：按键到不了 Windows 程序。请用 Windows Terminal，或者运行 `winpty zcode.cmd`。"
        )
    );
}

/// 本机（测试在 macOS / Linux 上跑）没有控制台这一层：是终端就能进入。
#[cfg(not(windows))]
#[test]
fn unix_has_no_console_layer() {
    assert!(console_input());
}
