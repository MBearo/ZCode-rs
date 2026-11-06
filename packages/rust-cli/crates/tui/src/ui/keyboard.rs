//! 终端按键的入口归一（docs/specs/rust-cli/tui-input.md §9.1，修复线 F65；架构整理 U9）。
//!
//! - [`normalize`]：Windows 把 AltGr 报成 Ctrl+Alt，crossterm 照样给出 Ctrl+Alt 加产生的字符：Windows 上它是文字。入口
//!   去掉这两个修饰键，之后处理按键的地方只看普通修饰键（[`ctrl`]、[`alt`]、[`is_text`]、[`is_plain_text`]）。
//! - [`Keyboard`]：主界面的入口：松开事件不进界面（Windows 的 alt+小键盘码例外，crossterm 把字符放在 Alt 键的松开事件
//!   里），再归一。
//!
//! 架构整理 U9：以前按平台的判断 `Typing` 由 `App` 持有、经参数传给列表、询问框与各面板，另有 10 处直接读 `CONTROL`，
//! 两种读法在不经 paste burst 的入口上不一致。现在只在入口判断一次。
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::os::Os;

/// Windows 的 AltGr（Ctrl+Alt 加一个可打印字符）去掉 Ctrl 与 Alt；其他按键与其他平台原样返回。
pub fn normalize(key: KeyEvent, os: Os) -> KeyEvent {
    let altgr = key
        .modifiers
        .contains(KeyModifiers::CONTROL | KeyModifiers::ALT)
        && matches!(key.code, KeyCode::Char(c) if !c.is_control());
    if os.is_windows() && altgr {
        KeyEvent {
            modifiers: key.modifiers - (KeyModifiers::CONTROL | KeyModifiers::ALT),
            ..key
        }
    } else {
        key
    }
}

/// 快捷键看的 ctrl（入口归一之后，AltGr 不带它）。
pub fn ctrl(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
}

/// 快捷键看的 alt。
pub fn alt(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::ALT)
}

/// 文字输入：字符键且没有 ctrl（alt+字符也算，输入框等原来只排除 ctrl 的地方）。
pub fn is_text(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char(_)) && !ctrl(key)
}

/// 文字输入，并且没有 alt（`/resume`、项目页的输入，以及 paste burst 的判定）。
pub fn is_plain_text(key: &KeyEvent) -> bool {
    is_text(key) && !alt(key)
}

/// 主界面的按键入口：松开事件的过滤（Windows 上配对按下与松开，认出 alt 码）与 AltGr 的归一。
#[derive(Debug, Clone, Default)]
pub struct Keyboard {
    os: Os,
    /// Windows：按下了、还没松开的字符键（按按下的顺序）。
    held: Vec<char>,
    /// Windows：启动或焦点变化之后还没有按下过键。这时没有配对的松开是在别处按下的键（别的窗口、启动前的选择屏），
    /// 不是 alt 码。
    refocused: bool,
}

impl Keyboard {
    pub fn new(os: Os) -> Self {
        Self {
            os,
            held: Vec::new(),
            refocused: true,
        }
    }

    /// 进入界面的按键：按下与重复照常，AltGr 归一（[`normalize`]）；松开事件丢掉，Windows 上没有配对的带字符的松开
    /// （alt 码）当作这个字符的按下。
    ///
    /// 缺陷修复（F65 的 T3）：crossterm 在 Windows 上把 alt+小键盘码产生的字符放在 Alt 键的松开事件里（前面没有这个
    /// 字符的按下事件），以前一律丢掉松开事件，这些字符打不出来。别的字符键的松开都有按下与之配对，配对的丢掉。
    pub fn admit(&mut self, key: KeyEvent) -> Option<KeyEvent> {
        self.release(key).map(|key| normalize(key, self.os))
    }

    fn release(&mut self, key: KeyEvent) -> Option<KeyEvent> {
        if !self.os.is_windows() {
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
