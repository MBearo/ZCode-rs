use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::{Keyboard, alt, ctrl, is_plain_text, is_text, normalize};
use crate::os::{Os, WindowsRelease};

const WINDOWS: Os = Os::Windows(Some(WindowsRelease {
    major: 10,
    build: 22631,
}));
const CTRL_ALT: KeyModifiers = KeyModifiers::CONTROL.union(KeyModifiers::ALT);

fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, modifiers)
}

fn up(c: char, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent::new_with_kind(KeyCode::Char(c), modifiers, KeyEventKind::Release)
}

/// T2：Windows 把 AltGr 报成 Ctrl+Alt（德语键盘的 `@`、`{`，波兰语的 `ł`）。Windows 上入口把它们归一成文字，ctrl 与
/// alt 都不算；其他平台照旧是 ctrl 组合。
#[test]
fn altgr_characters_are_text_only_on_windows() {
    for c in ['@', '{', '\\', '€', 'ł'] {
        let altgr = key(KeyCode::Char(c), CTRL_ALT);
        let windows = normalize(altgr, WINDOWS);
        assert!(is_text(&windows), "{c}");
        assert!(is_plain_text(&windows), "{c}");
        assert!(!ctrl(&windows) && !alt(&windows), "{c}");
        let unix = normalize(altgr, Os::Unix);
        assert!(!is_text(&unix), "{c}");
        assert!(ctrl(&unix) && alt(&unix), "{c}");
    }
    // 读不到系统版本也是 Windows；shift 留着。
    let shifted = key(KeyCode::Char('@'), CTRL_ALT | KeyModifiers::SHIFT);
    assert_eq!(
        normalize(shifted, Os::Windows(None)).modifiers,
        KeyModifiers::SHIFT
    );
}

/// 只有 Ctrl+Alt 加可打印字符算 AltGr：单独的 ctrl、单独的 alt、Ctrl+Alt 加方向键照旧。
#[test]
fn other_modifiers_keep_their_meaning_on_windows() {
    let ctrl_c = normalize(key(KeyCode::Char('c'), KeyModifiers::CONTROL), WINDOWS);
    assert!(!is_text(&ctrl_c) && ctrl(&ctrl_c));
    let alt_b = normalize(key(KeyCode::Char('b'), KeyModifiers::ALT), WINDOWS);
    assert!(is_text(&alt_b) && !is_plain_text(&alt_b));
    assert!(alt(&alt_b));
    let left = normalize(key(KeyCode::Left, CTRL_ALT), WINDOWS);
    assert!(!is_text(&left));
    assert!(ctrl(&left) && alt(&left));
    let shifted = normalize(key(KeyCode::Char('A'), KeyModifiers::SHIFT), WINDOWS);
    assert!(is_plain_text(&shifted));
    let control = normalize(key(KeyCode::Char('\u{1}'), CTRL_ALT), WINDOWS);
    assert!(!is_text(&control));
}

/// 主界面的入口归一：AltGr 的按下去掉 Ctrl+Alt，松开照样配对丢掉。
#[test]
fn the_entry_normalizes_altgr() {
    let mut keyboard = Keyboard::new(WINDOWS);
    assert_eq!(
        keyboard.admit(key(KeyCode::Char('@'), CTRL_ALT)),
        Some(key(KeyCode::Char('@'), KeyModifiers::NONE))
    );
    assert_eq!(keyboard.admit(up('@', CTRL_ALT)), None);
    let mut unix = Keyboard::new(Os::Unix);
    let altgr = key(KeyCode::Char('@'), CTRL_ALT);
    assert_eq!(unix.admit(altgr), Some(altgr));
}

/// T3：Windows 上每个键都有按下与松开；配对的松开丢掉。alt+小键盘码的字符只在 Alt 的松开事件里，没有配对：
/// 当作这个字符的按下（不带修饰键）。
#[test]
fn alt_codes_come_through_and_paired_releases_are_dropped() {
    let mut keyboard = Keyboard::new(WINDOWS);
    let a = key(KeyCode::Char('a'), KeyModifiers::NONE);
    assert_eq!(keyboard.admit(a), Some(a));
    assert_eq!(keyboard.admit(up('a', KeyModifiers::NONE)), None);
    assert_eq!(
        keyboard.admit(up('ä', KeyModifiers::NONE)),
        Some(key(KeyCode::Char('ä'), KeyModifiers::NONE))
    );
    // Alt 的状态还在时也一样，修饰键去掉。
    assert_eq!(
        keyboard.admit(up('é', KeyModifiers::ALT)),
        Some(key(KeyCode::Char('é'), KeyModifiers::NONE))
    );
    // 非字符键的松开照样丢掉。
    let enter = KeyEvent::new_with_kind(KeyCode::Enter, KeyModifiers::NONE, KeyEventKind::Release);
    assert_eq!(keyboard.admit(enter), None);
}

/// 按下与松开的字符不同时也配对：shift 先松开（`A` / `a`）、AltGr 先松开（`@` / `q`）、两个键交错、按住重复。
#[test]
fn releases_pair_with_presses_whose_character_changed() {
    let mut keyboard = Keyboard::new(WINDOWS);
    keyboard.admit(key(KeyCode::Char('A'), KeyModifiers::SHIFT));
    assert_eq!(keyboard.admit(up('a', KeyModifiers::NONE)), None);
    keyboard.admit(key(KeyCode::Char('@'), CTRL_ALT));
    assert_eq!(keyboard.admit(up('q', KeyModifiers::NONE)), None);
    keyboard.admit(key(KeyCode::Char('x'), KeyModifiers::NONE));
    keyboard.admit(key(KeyCode::Char('y'), KeyModifiers::NONE));
    assert_eq!(keyboard.admit(up('x', KeyModifiers::NONE)), None);
    assert_eq!(keyboard.admit(up('y', KeyModifiers::NONE)), None);
    for _ in 0..3 {
        keyboard.admit(key(KeyCode::Char('z'), KeyModifiers::NONE));
    }
    assert_eq!(keyboard.admit(up('z', KeyModifiers::NONE)), None);
    // 都配对完了：下一个没有配对的是 alt 码。
    assert!(keyboard.admit(up('ñ', KeyModifiers::NONE)).is_some());
}

/// 启动与焦点变化之后、按下任何键之前，没有配对的松开是在别处按下的键（例如 Win+数字切过来时数字键的松开、启动前
/// 选择屏里按的数字），不是 alt 码。焦点变化也清掉在窗口外松开的键。
#[test]
fn stray_releases_after_start_or_focus_are_dropped() {
    let mut keyboard = Keyboard::new(WINDOWS);
    assert_eq!(keyboard.admit(up('1', KeyModifiers::NONE)), None);
    keyboard.admit(key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(keyboard.admit(up('ä', KeyModifiers::NONE)).is_some());
    keyboard.admit(key(KeyCode::Char('k'), KeyModifiers::NONE));
    keyboard.focus_changed();
    assert_eq!(keyboard.admit(up('2', KeyModifiers::NONE)), None);
    keyboard.admit(key(KeyCode::Char('b'), KeyModifiers::NONE));
    assert_eq!(keyboard.admit(up('b', KeyModifiers::NONE)), None);
    assert!(keyboard.admit(up('ö', KeyModifiers::NONE)).is_some());
}

/// 其他平台的松开事件一律丢掉（不变）。
#[test]
fn releases_are_dropped_elsewhere() {
    let mut keyboard = Keyboard::new(Os::Unix);
    keyboard.admit(key(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(keyboard.admit(up('ä', KeyModifiers::NONE)), None);
    let a = key(KeyCode::Char('a'), KeyModifiers::NONE);
    assert_eq!(keyboard.admit(a), Some(a));
}
