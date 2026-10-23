//! 运行的操作系统（入口注入，docs/specs/rust-cli/tui-input.md §9、tui.md §7.4，修复线 F65）：按键的含义
//! （AltGr、alt 码、没有括号粘贴）与颜色级别按它判断，tui 不用 `cfg!(windows)` 判断这些规则；测试在 macOS 上注入
//! Windows。

/// Windows 的系统版本（Node `os.release()` 的第一段与第三段，例如 `10.0.22631` 的 10 与 22631）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowsRelease {
    pub major: u32,
    pub build: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Os {
    /// macOS、Linux 等：终端经 VT 序列输入，有括号粘贴。
    #[default]
    Unix,
    /// Windows：crossterm 读控制台的输入记录。系统版本读不到时为 None。
    Windows(Option<WindowsRelease>),
}

impl Os {
    pub fn is_windows(self) -> bool {
        matches!(self, Self::Windows(_))
    }
}
