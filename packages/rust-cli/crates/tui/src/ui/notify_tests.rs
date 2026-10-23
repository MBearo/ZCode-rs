//! 终端通知的渠道、序列与计时（tui-manage.md §5）。时间用注入的 `Instant`，不睡眠。
use rust_cli_kernel_api::ids::ToolUseId;
use serde_json::json;

use super::*;

fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[test]
fn the_channel_follows_the_terminal() {
    assert_eq!(
        channel(&vars(&[("TERM_PROGRAM", "iTerm.app")])),
        Some(Channel::ITerm2)
    );
    assert_eq!(
        channel(&vars(&[("TERM_PROGRAM", "kitty")])),
        Some(Channel::Kitty)
    );
    assert_eq!(
        channel(&vars(&[("KITTY_WINDOW_ID", "1")])),
        Some(Channel::Kitty)
    );
    assert_eq!(
        channel(&vars(&[("TERM_PROGRAM", "ghostty")])),
        Some(Channel::Ghostty)
    );
    assert_eq!(channel(&vars(&[("TERM_PROGRAM", "Apple_Terminal")])), None);
    assert_eq!(channel(&vars(&[])), None);
}

#[test]
fn sequences_match_each_channel() {
    let none = vars(&[]);
    assert_eq!(
        sequence(Channel::ITerm2, "hi", &none, 7),
        "\x1b]9;\n\nhi\x07"
    );
    assert_eq!(
        sequence(Channel::Ghostty, "hi", &none, 7),
        "\x1b]777;notify;ZCode;hi\x07"
    );
    assert_eq!(
        sequence(Channel::Kitty, "hi", &none, 7),
        "\x1b]99;i=7:d=0:p=title;ZCode\x1b\\\x1b]99;i=7:p=body;hi\x1b\\\x1b]99;i=7:d=1:a=focus;\x1b\\"
    );
    assert_eq!(
        sequence(Channel::ITerm2, "hi", &vars(&[("TMUX", "/tmp/t")]), 7),
        "\x1bPtmux;\x1b\x1b]9;\n\nhi\x07\x1b\\"
    );
    assert_eq!(
        sequence(Channel::ITerm2, "hi", &vars(&[("STY", "1.s")]), 7),
        "\x1bP\x1b]9;\n\nhi\x07\x1b\\"
    );
}

fn permission() -> InteractionRequest {
    InteractionRequest::Permission {
        tool_use: ToolUseId::new("toolu_1"),
        tool: "Bash".to_owned(),
        input: json!({"command": "ls"}),
        message: "Bash wants to run ls".to_owned(),
        suggestions: Vec::new(),
        asked_by: None,
    }
}

#[test]
fn a_dialog_notifies_after_six_quiet_seconds_once() {
    let start = Instant::now();
    let mut notify = Notify::default();
    let id = InteractionId::new("int_1");
    let request = permission();
    notify.dialog(Some((&id, &request)), Lang::En, start);
    assert_eq!(notify.next_due(), Some(start + DIALOG_AFTER));
    // 3 s 时按了键：6 s 的检查不发，12 s 再看。
    notify.input(start + Duration::from_secs(3));
    assert_eq!(notify.due(start + DIALOG_AFTER, false, Lang::En), None);
    assert_eq!(notify.next_due(), Some(start + DIALOG_AFTER * 2));
    let due = notify
        .due(start + DIALOG_AFTER * 2, false, Lang::En)
        .unwrap();
    assert_eq!(due.message, "ZCode needs your permission");
    assert_eq!(due.kind, "permission_prompt");
    assert_eq!(notify.next_due(), None);
    // 同一个交互不再提醒；交互没了就停。
    notify.dialog(Some((&id, &request)), Lang::En, start + DIALOG_AFTER * 3);
    assert_eq!(notify.next_due(), None);
    notify.dialog(None, Lang::En, start + DIALOG_AFTER * 3);
    assert_eq!(notify.next_due(), None);
}

#[test]
fn idle_waits_sixty_seconds_after_a_turn_and_for_quiet() {
    let start = Instant::now();
    let mut notify = Notify::default();
    notify.turn_finished(start);
    // 还没发过输入：不提醒。
    assert_eq!(notify.next_due(), None);
    notify.submitted();
    assert_eq!(notify.next_due(), Some(start + IDLE_AFTER));
    // 有后台子代理（不安静）：6 s 后再看。
    assert_eq!(notify.due(start + IDLE_AFTER, false, Lang::En), None);
    let later = start + IDLE_AFTER + DIALOG_AFTER;
    assert_eq!(notify.next_due(), Some(later));
    let due = notify.due(later, true, Lang::Zh).unwrap();
    assert_eq!(due.message, "ZCode 正在等你输入");
    assert_eq!(due.kind, "idle_prompt");
    assert_eq!(notify.next_due(), None);
    // 结束之后按过键：这次不提醒。
    notify.turn_finished(later);
    notify.input(later + Duration::from_secs(1));
    assert_eq!(notify.due(later + IDLE_AFTER, true, Lang::En), None);
    assert_eq!(notify.next_due(), None);
}

/// kitty 通知的 `i=` 在 0–9999，由注入的 Ids 给的随机 ID 折出；同一个种子
/// 得同一个值（F7：以前直接取 uuid）。
#[test]
fn notification_ids_fold_a_random_id_into_four_digits() {
    for seed in ["", "u1", "6f1c1f9e-1c1a-4b7e-9a51-1f0c3b9a2d7e", "cmd_3"] {
        assert!(id(seed) < 10_000, "{seed}");
        assert_eq!(id(seed), id(seed));
    }
    assert_ne!(id("u1"), id("u2"));
}
