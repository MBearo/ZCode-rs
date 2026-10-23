use std::collections::BTreeMap;
use std::time::Duration;

use super::*;

fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[test]
fn osc52_follows_the_multiplexer_and_the_terminal() {
    // `aGk=` 是 “hi” 的 base64。
    assert_eq!(osc52("hi", &vars(&[])), "\x1b]52;c;aGk=\x07");
    assert_eq!(
        osc52("hi", &vars(&[("TERM", "xterm-kitty")])),
        "\x1b]52;c;aGk=\x1b\\"
    );
    assert_eq!(
        osc52("hi", &vars(&[("TMUX", "/tmp/tmux-1/default,1,0")])),
        "\x1b]52;c;aGk=\x07\x1bPtmux;\x1b\x1b]52;c;aGk=\x07\x1b\\"
    );
    assert_eq!(
        osc52("hi", &vars(&[("STY", "1.pts")])),
        "\x1bP\x1b]52;c;aGk=\x07\x1b\\"
    );
    // screen：base64 按 76 个字符分段，段之间 `ESC \ ESC P`。
    let long = "x".repeat(60);
    let sequence = osc52(&long, &vars(&[("STY", "1.pts")]));
    assert_eq!(sequence.matches("\x1b\\\x1bP").count(), 1, "{sequence:?}");
}

#[test]
fn the_native_key_depends_on_the_terminal() {
    let key = |pairs: &[(&str, &str)], macos| native_key(&vars(pairs), macos);
    assert_eq!(key(&[("TERM_PROGRAM", "Apple_Terminal")], true), "Fn");
    assert_eq!(key(&[("TERM_PROGRAM", "iTerm.app")], true), "Option");
    assert_eq!(key(&[("TERM_PROGRAM", "vscode")], true), "Option");
    assert_eq!(key(&[("TERM_PROGRAM", "vscode")], false), "Shift");
    assert_eq!(key(&[("TERM", "xterm-ghostty")], true), "Shift");
    assert_eq!(key(&[("WT_SESSION", "x")], false), "Shift");
    assert_eq!(key(&[("LC_TERMINAL", "iTerm2")], false), "Option");
    let long = "Shift (Option in iTerm2, Fn in Terminal.app)";
    assert_eq!(key(&[("SSH_CONNECTION", "a b c d")], false), long);
    assert_eq!(key(&[], true), long);
    assert_eq!(key(&[], false), "Shift");
    // tmux 里终端名是 tmux（不在 Shift 的终端名单里），按复用器给长写法。
    assert_eq!(key(&[("TMUX", "/tmp/t,1,0")], false), long);
}

#[test]
fn only_ssh_connection_means_ssh() {
    assert!(ssh(&vars(&[("SSH_CONNECTION", "a b c d")])));
    assert!(!ssh(&vars(&[("SSH_TTY", "/dev/ttys001")])));
}

#[test]
fn notices_use_the_fixed_text_and_durations() {
    let none = vars(&[]);
    let native = notice(CopyRoute::Native, "hello", &none, Lang::En);
    assert_eq!(native.text, "copied 5 chars to clipboard");
    assert_eq!(native.duration, Duration::from_secs(2));
    assert_eq!(
        notice(CopyRoute::Native, "h", &none, Lang::En).text,
        "copied 1 char to clipboard"
    );
    let tmux = notice(CopyRoute::TmuxBuffer, "hello", &none, Lang::En);
    assert_eq!(
        tmux.text,
        "copied 5 chars to tmux buffer · paste with prefix + ]"
    );
    assert_eq!(tmux.duration, Duration::from_secs(4));
    let terminal = vars(&[("TERM_PROGRAM", "Apple_Terminal")]);
    assert_eq!(
        notice(CopyRoute::Osc52, "hello", &terminal, Lang::En).text,
        "sent 5 chars via OSC 52 · if paste fails, hold Fn while selecting for native copy"
    );
    assert_eq!(
        notice(CopyRoute::Native, "你好", &none, Lang::Zh).text,
        "已复制 2 个字符到剪贴板"
    );
}

#[test]
fn vs_code_1_123_gets_the_mojibake_warning_for_non_ascii() {
    let code = vars(&[
        ("TERM_PROGRAM", "vscode"),
        ("TERM_PROGRAM_VERSION", "1.124.2"),
    ]);
    let warned = notice(CopyRoute::Osc52, "héllo", &code, Lang::En);
    assert!(
        warned.text.starts_with(
            "⚠ VS Code 1.123/1.124 will mojibake this paste — update to ≥1.125 · sent 5 chars"
        ),
        "{}",
        warned.text
    );
    assert_eq!(warned.duration, Duration::from_secs(6));
    assert!(
        !notice(CopyRoute::Osc52, "hello", &code, Lang::En)
            .text
            .starts_with('⚠')
    );
    let newer = vars(&[
        ("TERM_PROGRAM", "vscode"),
        ("TERM_PROGRAM_VERSION", "1.125.0"),
    ]);
    assert!(
        !notice(CopyRoute::Osc52, "héllo", &newer, Lang::En)
            .text
            .starts_with('⚠')
    );
}
