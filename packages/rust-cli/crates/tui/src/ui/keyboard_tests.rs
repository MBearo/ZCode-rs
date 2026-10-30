use std::time::{Duration, Instant};

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
    let now = Instant::now();
    let a = key(KeyCode::Char('a'), KeyModifiers::NONE);
    assert_eq!(releases.admit(a, now), Some(a));
    assert_eq!(releases.admit(up('a', KeyModifiers::NONE), now), None);
    assert_eq!(
        releases.admit(up('ä', KeyModifiers::NONE), now),
        Some(key(KeyCode::Char('ä'), KeyModifiers::NONE))
    );
    // Alt 的状态还在时也一样，修饰键去掉。
    assert_eq!(
        releases.admit(up('é', KeyModifiers::ALT), now),
        Some(key(KeyCode::Char('é'), KeyModifiers::NONE))
    );
    // 非字符键的松开照样丢掉。
    let enter = KeyEvent::new_with_kind(KeyCode::Enter, KeyModifiers::NONE, KeyEventKind::Release);
    assert_eq!(releases.admit(enter, now), None);
}

/// 按下与松开的字符不同时也配对：shift 先松开（`A` / `a`）、AltGr 先松开（`@` / `q`）、两个键交错、按住重复。
#[test]
fn releases_pair_with_presses_whose_character_changed() {
    let mut releases = Releases::new(WINDOWS);
    let now = Instant::now();
    releases.admit(key(KeyCode::Char('A'), KeyModifiers::SHIFT), now);
    assert_eq!(releases.admit(up('a', KeyModifiers::NONE), now), None);
    releases.admit(key(KeyCode::Char('@'), CTRL_ALT), now);
    assert_eq!(releases.admit(up('q', KeyModifiers::NONE), now), None);
    releases.admit(key(KeyCode::Char('x'), KeyModifiers::NONE), now);
    releases.admit(key(KeyCode::Char('y'), KeyModifiers::NONE), now);
    assert_eq!(releases.admit(up('x', KeyModifiers::NONE), now), None);
    assert_eq!(releases.admit(up('y', KeyModifiers::NONE), now), None);
    for _ in 0..3 {
        releases.admit(key(KeyCode::Char('z'), KeyModifiers::NONE), now);
    }
    assert_eq!(releases.admit(up('z', KeyModifiers::NONE), now), None);
    // 都配对完了：下一个没有配对的是 alt 码。
    assert!(releases.admit(up('ñ', KeyModifiers::NONE), now).is_some());
}

/// 启动与焦点变化之后、按下任何键之前，没有配对的松开是在别处按下的键（例如 Win+数字切过来时数字键的松开、启动前
/// 选择屏里按的数字），不是 alt 码。焦点变化也清掉在窗口外松开的键。
#[test]
fn stray_releases_after_start_or_focus_are_dropped() {
    let mut releases = Releases::new(WINDOWS);
    let now = Instant::now();
    assert_eq!(releases.admit(up('1', KeyModifiers::NONE), now), None);
    releases.admit(key(KeyCode::Enter, KeyModifiers::NONE), now);
    assert!(releases.admit(up('ä', KeyModifiers::NONE), now).is_some());
    releases.admit(key(KeyCode::Char('k'), KeyModifiers::NONE), now);
    releases.focus_changed();
    assert_eq!(releases.admit(up('2', KeyModifiers::NONE), now), None);
    releases.admit(key(KeyCode::Char('b'), KeyModifiers::NONE), now);
    assert_eq!(releases.admit(up('b', KeyModifiers::NONE), now), None);
    assert!(releases.admit(up('ö', KeyModifiers::NONE), now).is_some());
}

/// 其他平台的松开事件一律丢掉（不变）。
#[test]
fn releases_are_dropped_elsewhere() {
    let mut releases = Releases::new(Os::Unix);
    let now = Instant::now();
    releases.admit(key(KeyCode::Enter, KeyModifiers::NONE), now);
    assert_eq!(releases.admit(up('ä', KeyModifiers::NONE), now), None);
    let a = key(KeyCode::Char('a'), KeyModifiers::NONE);
    assert_eq!(releases.admit(a, now), Some(a));
}

/// 输入法确认键的按下透传（实测定案的序列）：提交的非 ASCII 字符以按下加松开对注入（正常键入），确认键（空格选词、
/// 半角标点、数字选词）的原始按键随后又作为按下加松开对透传。窗口内的首个 ASCII 按下被吸收（连带配对的松开静默
/// 丢弃）：中文标点后不再多一个英文标点、空格选词后不再多一个空格、数字选词后不再多一个数字；连续提交互不影响。
#[test]
fn ime_commit_confirmation_press_echo_is_absorbed() {
    let mut releases = Releases::new(WINDOWS);
    let t0 = Instant::now();
    let soon = Duration::from_millis(1);
    // 空格选词：提交「你好」注入对，随后确认键空格的按下与松开。
    let ni = key(KeyCode::Char('你'), KeyModifiers::NONE);
    let hao = key(KeyCode::Char('好'), KeyModifiers::NONE);
    assert_eq!(releases.admit(ni, t0), Some(ni));
    assert_eq!(releases.admit(hao, t0), Some(hao));
    assert_eq!(releases.admit(up('好', KeyModifiers::NONE), t0), None);
    assert_eq!(releases.admit(up('你', KeyModifiers::NONE), t0), None);
    assert_eq!(
        releases.admit(key(KeyCode::Char(' '), KeyModifiers::NONE), t0 + soon),
        None
    );
    assert_eq!(releases.admit(up(' ', KeyModifiers::NONE), t0 + soon), None);
    // 中文标点：提交「，」，随后半角逗号的按下与松开。
    let comma = key(KeyCode::Char('，'), KeyModifiers::NONE);
    assert_eq!(releases.admit(comma, t0 + soon * 2), Some(comma));
    assert_eq!(
        releases.admit(up('，', KeyModifiers::NONE), t0 + soon * 2),
        None
    );
    assert_eq!(
        releases.admit(key(KeyCode::Char(','), KeyModifiers::NONE), t0 + soon * 3),
        None
    );
    assert_eq!(
        releases.admit(up(',', KeyModifiers::NONE), t0 + soon * 3),
        None
    );
    // 数字选词同理。
    let ci = key(KeyCode::Char('词'), KeyModifiers::NONE);
    assert_eq!(releases.admit(ci, t0 + soon * 4), Some(ci));
    assert_eq!(
        releases.admit(up('词', KeyModifiers::NONE), t0 + soon * 4),
        None
    );
    assert_eq!(
        releases.admit(key(KeyCode::Char('2'), KeyModifiers::NONE), t0 + soon * 5),
        None
    );
    assert_eq!(
        releases.admit(up('2', KeyModifiers::NONE), t0 + soon * 5),
        None
    );
}

/// 窗口内带 ctrl/alt 的组合（Windows 的 AltGr 报成 Ctrl+Alt 加字符）不是确认键回声，不吸收：AltGr 的真实符号键入
/// 不因刚提交过中文而丢失。
#[test]
fn altgr_within_ime_window_is_not_absorbed() {
    let mut releases = Releases::new(WINDOWS);
    let t0 = Instant::now();
    let ci = key(KeyCode::Char('中'), KeyModifiers::NONE);
    assert_eq!(releases.admit(ci, t0), Some(ci));
    let at = key(KeyCode::Char('@'), CTRL_ALT);
    assert_eq!(releases.admit(at, t0 + Duration::from_millis(1)), Some(at));
    assert_eq!(
        releases.admit(up('@', KeyModifiers::NONE), t0 + Duration::from_millis(1)),
        None
    );
}

/// 窗口之外与无提交时不受影响：英文连续打字（没有非 ASCII 提交过）照常，距提交超过窗口的按键照常放行；
/// 窗口已过时孤儿 ASCII 松开也照旧合成（alt 码路径的现行为）。
#[test]
fn stray_ascii_release_outside_ime_window_still_admitted() {
    let mut releases = Releases::new(WINDOWS);
    let t0 = Instant::now();
    // 英文连续打字：没有提交过非 ASCII，按下与松开照常。
    let a = key(KeyCode::Char('a'), KeyModifiers::NONE);
    assert_eq!(releases.admit(a, t0), Some(a));
    assert_eq!(releases.admit(up('a', KeyModifiers::NONE), t0), None);
    let ni = key(KeyCode::Char('你'), KeyModifiers::NONE);
    assert_eq!(releases.admit(ni, t0), Some(ni));
    assert_eq!(releases.admit(up('你', KeyModifiers::NONE), t0), None);
    // 距提交超过窗口：ASCII 按下照常放行。
    let later = t0 + Duration::from_millis(60);
    let one = key(KeyCode::Char('1'), KeyModifiers::NONE);
    assert_eq!(releases.admit(one, later), Some(one));
    assert_eq!(releases.admit(up('1', KeyModifiers::NONE), later), None);
    // 孤儿松开在窗口外也照旧合成（alt 码）。
    assert_eq!(
        releases.admit(up('ä', KeyModifiers::NONE), later),
        Some(key(KeyCode::Char('ä'), KeyModifiers::NONE))
    );
}

/// 实测取证的回归（%TEMP% 事件日志，2026-10-30）：确认键的孤儿 ASCII 松开距提交 33–73ms 浮动——51ms（中文标点）、
/// 52/57/64ms（空格选词）在 50ms 窗口下泄漏、33/42ms 被吞。窗口放宽到 150ms 后全部覆盖：中文标点后无半角跟随、
/// 空格选词后无多余空格。
#[test]
fn ime_release_echo_at_measured_delays_is_dropped() {
    for delay_ms in [33, 42, 51, 52, 57, 64, 73] {
        let mut releases = Releases::new(WINDOWS);
        let t0 = Instant::now();
        let delay = Duration::from_millis(delay_ms);
        let juhao = key(KeyCode::Char('。'), KeyModifiers::NONE);
        assert_eq!(releases.admit(juhao, t0), Some(juhao), "{delay_ms}ms");
        assert_eq!(
            releases.admit(up('。', KeyModifiers::NONE), t0),
            None,
            "{delay_ms}ms"
        );
        assert_eq!(
            releases.admit(up('.', KeyModifiers::NONE), t0 + delay),
            None,
            "delay {delay_ms}ms"
        );
        // 空格选词同窗口。
        let ni = key(KeyCode::Char('你'), KeyModifiers::NONE);
        let t1 = t0 + delay * 2;
        assert_eq!(releases.admit(ni, t1), Some(ni), "{delay_ms}ms");
        assert_eq!(
            releases.admit(up('你', KeyModifiers::NONE), t1),
            None,
            "{delay_ms}ms"
        );
        assert_eq!(
            releases.admit(up(' ', KeyModifiers::NONE), t1 + delay),
            None,
            "delay {delay_ms}ms"
        );
    }
}
