//! 进入全屏界面之前对终端的判断（docs/specs/rust-cli/tui-input.md §9.6，工作线 F71）：TUI 与启动前的两个选择屏
//! （工作区信任、自动更新）共用。
//!
//! 缺陷修复（T6）：Git Bash 自带的 mintty 不是 Windows 控制台，原生程序的 stdin / stdout 是 msys 的 pty（命名管道）。
//! Rust 的 `is_terminal` 认这种管道是终端，以前照常进入界面；crossterm 却只读控制台输入，按键到不了程序。现在先看
//! stdin 是不是控制台，不是时报错并提示 Windows Terminal 或 winpty。

use super::i18n::Strings;

/// stdin / stdout 能不能进入全屏界面。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Console {
    /// 都是终端，stdin 是控制台（Unix 上只看是不是终端）。
    Ready,
    /// 有一个不是终端（管道、文件）。
    NotTerminal,
    /// 都是终端，stdin 却不是 Windows 控制台：mintty 一类的 msys / cygwin pty。
    Mintty,
}

impl Console {
    /// `stdin_terminal` / `stdout_terminal` 是 `is_terminal` 的结果；`console_input` 是 stdin 是不是 Windows 控制台
    /// （Unix 上总是 true）。
    pub fn classify(stdin_terminal: bool, stdout_terminal: bool, console_input: bool) -> Self {
        if !stdin_terminal || !stdout_terminal {
            Self::NotTerminal
        } else if !console_input {
            Self::Mintty
        } else {
            Self::Ready
        }
    }

    /// 本进程的 stdin / stdout。
    pub fn current() -> Self {
        use std::io::IsTerminal;
        Self::classify(
            std::io::stdin().is_terminal(),
            std::io::stdout().is_terminal(),
            console_input(),
        )
    }

    /// 不能进入时给用户的报错；能进入时 None。
    pub fn refusal(self, strings: &Strings) -> Option<&'static str> {
        match self {
            Self::Ready => None,
            Self::NotTerminal => Some(strings.requires_terminal),
            Self::Mintty => Some(strings.mintty_unsupported),
        }
    }
}

/// stdin 是 Windows 控制台：`GetConsoleMode` 成功（msys 的 pty 是命名管道，这里失败）。
#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "GetStdHandle 与 GetConsoleMode 是 Win32 的 FFI，没有安全封装"
)]
fn console_input() -> bool {
    use windows_sys::Win32::System::Console::{GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE};
    let mut mode = 0;
    // SAFETY: 只读本进程 stdin 的句柄与模式；`mode` 是本函数里的局部变量。
    unsafe { GetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), &mut mode) != 0 }
}

/// Unix 上没有这一层：是终端就能读按键。
#[cfg(not(windows))]
fn console_input() -> bool {
    true
}

#[cfg(test)]
#[path = "console_tests.rs"]
mod tests;
