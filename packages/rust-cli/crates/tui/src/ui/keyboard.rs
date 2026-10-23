//! 文字输入的判断与松开事件（docs/specs/rust-cli/tui-input.md §9.1，修复线 F65）。
//!
//! - [`Typing`]：一个按键是不是文字输入、快捷键看的 ctrl 与 alt。Windows 把 AltGr 报成 Ctrl+Alt，crossterm 照样给出
//!   Ctrl+Alt 加产生的字符：Windows 上它是文字，不是 ctrl / alt 组合。
//! - [`Releases`]：松开事件不进界面；Windows 的 alt+小键盘码例外（crossterm 把字符放在 Alt 键的松开事件里）。
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::os::Os;

/// 按平台判断文字输入（入口注入的平台，见 `crate::os`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Typing {
    windows: bool,
}

impl Typing {
    pub fn new(os: Os) -> Self {
        Self {
            windows: os.is_windows(),
        }
    }

    /// Windows 的 AltGr：Ctrl+Alt 加一个可打印字符。
    fn altgr(self, key: &KeyEvent) -> bool {
        self.windows
            && key
                .modifiers
                .contains(KeyModifiers::CONTROL | KeyModifiers::ALT)
            && matches!(key.code, KeyCode::Char(c) if !c.is_control())
    }

    /// 快捷键看的 ctrl（AltGr 不算）。
    pub fn ctrl(self, key: &KeyEvent) -> bool {
        key.modifiers.contains(KeyModifiers::CONTROL) && !self.altgr(key)
    }

    /// 快捷键看的 alt（AltGr 不算）。
    pub fn alt(self, key: &KeyEvent) -> bool {
        key.modifiers.contains(KeyModifiers::ALT) && !self.altgr(key)
    }

    /// 文字输入：字符键且没有 ctrl（alt+字符也算，输入框等原来只排除 ctrl 的地方）。
    pub fn is_text(self, key: &KeyEvent) -> bool {
        matches!(key.code, KeyCode::Char(_)) && !self.ctrl(key)
    }

    /// 文字输入，并且没有 alt（`/resume`、项目页的输入，以及 paste burst 的判定）。
    pub fn is_plain_text(self, key: &KeyEvent) -> bool {
        self.is_text(key) && !self.alt(key)
    }
}

/// 松开事件的过滤；Windows 上配对按下与松开，认出 alt 码。
#[derive(Debug, Clone, Default)]
pub struct Releases {
    windows: bool,
    /// Windows：按下了、还没松开的字符键（按按下的顺序）。
    held: Vec<char>,
    /// Windows：启动或焦点变化之后还没有按下过键。这时没有配对的松开是在别处按下的键（别的窗口、启动前的选择屏），
    /// 不是 alt 码。
    refocused: bool,
}

impl Releases {
    pub fn new(os: Os) -> Self {
        Self {
            windows: os.is_windows(),
            held: Vec::new(),
            refocused: true,
        }
    }

    /// 进入界面的按键：按下与重复照常；松开事件丢掉，Windows 上没有配对的带字符的松开（alt 码）当作这个字符的按下。
    ///
    /// 缺陷修复（F65 的 T3）：crossterm 在 Windows 上把 alt+小键盘码产生的字符放在 Alt 键的松开事件里（前面没有这个
    /// 字符的按下事件），以前一律丢掉松开事件，这些字符打不出来。别的字符键的松开都有按下与之配对，配对的丢掉。
    pub fn admit(&mut self, key: KeyEvent) -> Option<KeyEvent> {
        if !self.windows {
            return (key.kind != KeyEventKind::Release).then_some(key);
        }
        let KeyCode::Char(c) = key.code else {
            if key.kind != KeyEventKind::Release {
                self.refocused = false;
                return Some(key);
            }
            return None;
        };
        if key.kind != KeyEventKind::Release {
            self.refocused = false;
            if !self.held.contains(&c) {
                self.held.push(c);
            }
            return Some(key);
        }
        // 先找同一个字符（大小写不同也算：shift 先松开时字符变了）；找不到时配对最早按下的（AltGr 先松开时字符变了）。
        let same = |held: &char| held.to_lowercase().eq(c.to_lowercase());
        if let Some(at) = self.held.iter().position(same) {
            self.held.remove(at);
            return None;
        }
        if !self.held.is_empty() {
            self.held.remove(0);
            return None;
        }
        if self.refocused {
            return None;
        }
        Some(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
    }

    /// 焦点变化：窗口外松开的键不留在记录里；之后第一次按键之前，没有配对的松开不当作 alt 码。
    pub fn focus_changed(&mut self) {
        self.held.clear();
        self.refocused = true;
    }
}

#[cfg(test)]
#[path = "keyboard_tests.rs"]
mod tests;
