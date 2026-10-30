//! 文字输入的判断与松开事件（docs/specs/rust-cli/tui-input.md §9.1，修复线 F65）。
//!
//! - [`Typing`]：一个按键是不是文字输入、快捷键看的 ctrl 与 alt。Windows 把 AltGr 报成 Ctrl+Alt，crossterm 照样给出
//!   Ctrl+Alt 加产生的字符：Windows 上它是文字，不是 ctrl / alt 组合。
//! - [`Releases`]：松开事件不进界面；Windows 的 alt+小键盘码例外（crossterm 把字符放在 Alt 键的松开事件里）。
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::os::Os;

/// 输入法提交之后认确认键松开回声的窗口。实测（%TEMP% 取证，2026-10-30）确认键（空格选词、半角标点）的孤儿松开
/// 距提交 33–73ms 浮动（输入法与终端的注入延迟），取 150ms 留足余量。窗口只作用于「孤儿松开合成放行」这条异常路径：
/// 正常打字的松开都有按下配对走不到这里，人工也无法在中文提交后 150ms 内敲出 alt 码，放得宽不误伤。
const IME_COMMIT_RELEASE_ECHO_WINDOW: Duration = Duration::from_millis(150);

/// 输入法提交之后认确认键按下回声的窗口：保险层（实测回声只以松开形态出现），拦的是正常按下路径，从严取 50ms，
/// 不吞人工续打。
const IME_COMMIT_PRESS_ECHO_WINDOW: Duration = Duration::from_millis(50);

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

/// 松开事件的过滤；Windows 上配对按下与松开，认出 alt 码与输入法确认键的回声。
#[derive(Debug, Clone, Default)]
pub struct Releases {
    windows: bool,
    /// Windows：按下了、还没松开的字符键（按按下的顺序）。
    held: Vec<char>,
    /// Windows：启动或焦点变化之后还没有按下过键。这时没有配对的松开是在别处按下的键（别的窗口、启动前的选择屏），
    /// 不是 alt 码。
    refocused: bool,
    /// Windows：最近一次放行的非 ASCII 字符按下（输入法提交）的时间，用于认出紧随其后的确认键回声。
    ime_committed_at: Option<Instant>,
}

impl Releases {
    pub fn new(os: Os) -> Self {
        Self {
            windows: os.is_windows(),
            held: Vec::new(),
            refocused: true,
            ime_committed_at: None,
        }
    }

    /// 进入界面的按键：按下与重复照常；松开事件丢掉，Windows 上没有配对的带字符的松开（alt 码）当作这个字符的按下。
    ///
    /// 缺陷修复（F65 的 T3）：crossterm 在 Windows 上把 alt+小键盘码产生的字符放在 Alt 键的松开事件里（前面没有这个
    /// 字符的按下事件），以前一律丢掉松开事件，这些字符打不出来。别的字符键的松开都有按下与之配对，配对的丢掉。
    ///
    /// 缺陷修复（输入法确认键透传）：Windows 的输入法（IME）提交时，提交的非 ASCII 字符以按下加松开对注入（正常键入），
    /// 确认键（空格选词、半角标点、数字选词）的**原始按键**随后又作为按下加松开对透传——它的按下被当作真实按键放行，
    /// 中文标点后多一个英文标点、空格选词后多一个空格、数字选词后多一个数字（英文与半角没有输入法，不受影响）。
    /// 现在提交后窗口内的首个 ASCII 按下判为确认键回声：吞掉并记入 held，让它配对的松开也静默丢弃；带 ctrl/alt 的
    /// 组合（AltGr 的真实符号）与窗口外的按键照旧。
    pub fn admit(&mut self, key: KeyEvent, now: Instant) -> Option<KeyEvent> {
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
            // 输入法确认键的按下回声：提交的非 ASCII 字符刚放行（窗口内），紧随的 ASCII 按下是确认键的原始按键，
            // 不是真实输入。只吞窗口内的首个；记入 held 让配对的松开也静默丢弃。ctrl/alt 组合（AltGr）不吞。
            if c.is_ascii()
                && !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                && let Some(committed_at) = self.ime_committed_at
                && now
                    .checked_duration_since(committed_at)
                    .is_some_and(|elapsed| elapsed <= IME_COMMIT_PRESS_ECHO_WINDOW)
            {
                self.ime_committed_at = None;
                if !self.held.contains(&c) {
                    self.held.push(c);
                }
                return None;
            }
            if !c.is_ascii() {
                self.ime_committed_at = Some(now);
            }
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
        // 输入法确认键的松开回声（实测形态）：提交的非 ASCII 字符刚放行，确认键的孤儿 ASCII 松开随后 33–73ms 到达
        // （按下被输入法消费）。只吞窗口内的首个，之后的照旧，真实按键的松开都有按下配对。
        if let Some(committed_at) = self.ime_committed_at
            && c.is_ascii()
            && now
                .checked_duration_since(committed_at)
                .is_some_and(|elapsed| elapsed <= IME_COMMIT_RELEASE_ECHO_WINDOW)
        {
            self.ime_committed_at = None;
            return None;
        }
        Some(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
    }

    /// 焦点变化：窗口外松开的键不留在记录里；之后第一次按键之前，没有配对的松开不当作 alt 码。
    pub fn focus_changed(&mut self) {
        self.held.clear();
        self.refocused = true;
        self.ime_committed_at = None;
    }
}

#[cfg(test)]
#[path = "keyboard_tests.rs"]
mod tests;
