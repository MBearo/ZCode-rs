use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::{Releases, Typing};
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

/// T2：Windows 把 AltGr 报成 Ctrl+Alt（德语键盘的 `@`、`{`，波兰语的 `ł`）。Windows 上它们是文字，ctrl 与 alt 都不算；
/// 其他平台照旧是 ctrl 组合。
#[test]
fn altgr_characters_are_text_only_on_windows() {
    let windows = Typing::new(WINDOWS);
    let unix = Typing::new(Os::Unix);
    for c in ['@', '{', '\\', '€', 'ł'] {
        let altgr = key(KeyCode::Char(c), CTRL_ALT);
        assert!(windows.is_text(&altgr), "{c}");
        assert!(windows.is_plain_text(&altgr), "{c}");
        assert!(!windows.ctrl(&altgr) && !windows.alt(&altgr), "{c}");
        assert!(!unix.is_text(&altgr), "{c}");
        assert!(unix.ctrl(&altgr) && unix.alt(&altgr), "{c}");
    }
    // 读不到系统版本也是 Windows。
    assert!(Typing::new(Os::Windows(None)).is_text(&key(KeyCode::Char('@'), CTRL_ALT)));
}

/// 只有 Ctrl+Alt 加可打印字符算 AltGr：单独的 ctrl、单独的 alt、Ctrl+Alt 加方向键照旧。
#[test]
fn other_modifiers_keep_their_meaning_on_windows() {
    let windows = Typing::new(WINDOWS);
    let ctrl_c = key(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert!(!windows.is_text(&ctrl_c) && windows.ctrl(&ctrl_c));
    let alt_b = key(KeyCode::Char('b'), KeyModifiers::ALT);
    assert!(windows.is_text(&alt_b) && !windows.is_plain_text(&alt_b));
    assert!(windows.alt(&alt_b));
    let left = key(KeyCode::Left, CTRL_ALT);
    assert!(!windows.is_text(&left));
    assert!(windows.ctrl(&left) && windows.alt(&left));
    let shifted = key(KeyCode::Char('A'), KeyModifiers::SHIFT);
    assert!(windows.is_plain_text(&shifted));
    let control = key(KeyCode::Char('\u{1}'), CTRL_ALT);
    assert!(!windows.is_text(&control));
}

/// T3：Windows 上每个键都有按下与松开；配对的松开丢掉。alt+小键盘码的字符只在 Alt 的松开事件里，没有配对：
/// 当作这个字符的按下（不带修饰键）。
#[test]
fn alt_codes_come_through_and_paired_releases_are_dropped() {
    let mut releases = Releases::new(WINDOWS);
    let a = key(KeyCode::Char('a'), KeyModifiers::NONE);
    assert_eq!(releases.admit(a), Some(a));
    assert_eq!(releases.admit(up('a', KeyModifiers::NONE)), None);
    assert_eq!(
        releases.admit(up('ä', KeyModifiers::NONE)),
        Some(key(KeyCode::Char('ä'), KeyModifiers::NONE))
    );
    // Alt 的状态还在时也一样，修饰键去掉。
    assert_eq!(
        releases.admit(up('é', KeyModifiers::ALT)),
        Some(key(KeyCode::Char('é'), KeyModifiers::NONE))
    );
    // 非字符键的松开照样丢掉。
    let enter = KeyEvent::new_with_kind(KeyCode::Enter, KeyModifiers::NONE, KeyEventKind::Release);
    assert_eq!(releases.admit(enter), None);
}

/// 按下与松开的字符不同时也配对：shift 先松开（`A` / `a`）、AltGr 先松开（`@` / `q`）、两个键交错、按住重复。
#[test]
fn releases_pair_with_presses_whose_character_changed() {
    let mut releases = Releases::new(WINDOWS);
    releases.admit(key(KeyCode::Char('A'), KeyModifiers::SHIFT));
    assert_eq!(releases.admit(up('a', KeyModifiers::NONE)), None);
    releases.admit(key(KeyCode::Char('@'), CTRL_ALT));
    assert_eq!(releases.admit(up('q', KeyModifiers::NONE)), None);
    releases.admit(key(KeyCode::Char('x'), KeyModifiers::NONE));
    releases.admit(key(KeyCode::Char('y'), KeyModifiers::NONE));
    assert_eq!(releases.admit(up('x', KeyModifiers::NONE)), None);
    assert_eq!(releases.admit(up('y', KeyModifiers::NONE)), None);
    for _ in 0..3 {
        releases.admit(key(KeyCode::Char('z'), KeyModifiers::NONE));
    }
    assert_eq!(releases.admit(up('z', KeyModifiers::NONE)), None);
    // 都配对完了：下一个没有配对的是 alt 码。
    assert!(releases.admit(up('ñ', KeyModifiers::NONE)).is_some());
}

/// 启动与焦点变化之后、按下任何键之前，没有配对的松开是在别处按下的键（例如 Win+数字切过来时数字键的松开、启动前
/// 选择屏里按的数字），不是 alt 码。焦点变化也清掉在窗口外松开的键。
#[test]
fn stray_releases_after_start_or_focus_are_dropped() {
    let mut releases = Releases::new(WINDOWS);
    assert_eq!(releases.admit(up('1', KeyModifiers::NONE)), None);
    releases.admit(key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(releases.admit(up('ä', KeyModifiers::NONE)).is_some());
    releases.admit(key(KeyCode::Char('k'), KeyModifiers::NONE));
    releases.focus_changed();
    assert_eq!(releases.admit(up('2', KeyModifiers::NONE)), None);
    releases.admit(key(KeyCode::Char('b'), KeyModifiers::NONE));
    assert_eq!(releases.admit(up('b', KeyModifiers::NONE)), None);
    assert!(releases.admit(up('ö', KeyModifiers::NONE)).is_some());
}

/// 其他平台的松开事件一律丢掉（不变）。
#[test]
fn releases_are_dropped_elsewhere() {
    let mut releases = Releases::new(Os::Unix);
    releases.admit(key(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(releases.admit(up('ä', KeyModifiers::NONE)), None);
    let a = key(KeyCode::Char('a'), KeyModifiers::NONE);
    assert_eq!(releases.admit(a), Some(a));
}
