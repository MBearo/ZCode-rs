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
        machine: Default::default(),
        tool_use: ToolUseId::new("toolu_1"),
        tool: "Bash".to_owned(),
        input: json!({"command": "ls"}),
        message: "Bash wants to run ls".to_owned(),
        suggestions: Vec::new(),
        rule_seed: None,
        asked_by: None,
    }
}

/// 照 Puffin（`asks.rs` 的 `asked`）：终端有焦点时请求到达不响；失去焦点后每个新到的请求响一次，标题前加 `● `；请求都离开
/// 后恢复标题。
#[test]
fn asks_ring_only_when_the_terminal_is_away() {
    let mut notify = Notify::default();
    let request = permission();
    assert_eq!(notify.asked(&[&request], false, true, Lang::En), None);
    assert_eq!(
        notify.title_bytes("Fix the build"),
        Some("\x1b[22;0t\x1b]0;Fix the build\x07".to_owned())
    );
    notify.focus(false, true);
    assert_eq!(
        notify.asked(&[&request], false, true, Lang::En),
        Some("ZCode needs your permission")
    );
    assert_eq!(
        notify.title_bytes("Fix the build"),
        Some("\x1b]0;● Fix the build\x07".to_owned())
    );
    // 没有新到的：不再响。
    assert_eq!(notify.asked(&[], false, true, Lang::En), None);
    assert_eq!(notify.title_bytes("Fix the build"), None);
    // 回到终端时请求还开着：标题留着；请求都离开：恢复。
    notify.focus(true, true);
    assert_eq!(notify.title_bytes("Fix the build"), None);
    assert_eq!(notify.asked(&[], false, false, Lang::En), None);
    assert_eq!(
        notify.title_bytes("Fix the build"),
        Some("\x1b]0;Fix the build\x07".to_owned())
    );
    // 没有会话的标题：不在终端前时写 `● ZCode`，之后弹回终端原来的标题。
    notify.focus(false, false);
    notify.asked(&[&request], false, true, Lang::Zh);
    assert_eq!(
        notify.title_bytes(""),
        Some("\x1b]0;● ZCode\x07".to_owned())
    );
    notify.asked(&[], false, false, Lang::Zh);
    assert_eq!(notify.title_bytes(""), Some("\x1b[23;0t".to_owned()));
    assert_eq!(notify.restore_title(), None);
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
    // 有后台子代理（不安静）：过一会儿再看。
    assert_eq!(notify.due(start + IDLE_AFTER, false, Lang::En), None);
    let later = start + IDLE_AFTER + RECHECK;
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

fn row(id: u64, body: Body) -> Row {
    Row {
        id,
        version: 0,
        created_ms: 0,
        body,
    }
}

fn peer(name: &str) -> Body {
    Body::Peer(Box::new(PeerRow::Message {
        name: name.into(),
        body: "hello".into(),
    }))
}

/// 照 Puffin（`notices::rings`、`asked` 的 `notice_unseen`）：终端在后台时新到的同伴消息与目标的结论响铃（说法是通知的），
/// 回执与系统提示不响；打开会话时已有的行不响；回到终端之前标题的 `●` 留着。
#[test]
fn notices_ring_while_away_and_the_title_waits_for_the_terminal() {
    let mut notify = Notify::default();
    let opened = [row(1, peer("work-b"))];
    // 打开会话：已有的行只记下。
    assert!(!notify.fresh_rows(Some("s1"), &opened));
    let receipt = Body::Peer(Box::new(PeerRow::Receipt {
        status: "held".into(),
    }));
    let mut rows = opened.to_vec();
    rows.push(row(2, receipt));
    assert!(
        !notify.fresh_rows(Some("s1"), &rows),
        "a receipt does not ring"
    );
    rows.push(row(3, peer("work-c")));
    assert!(notify.fresh_rows(Some("s1"), &rows));
    // 在前台：不响。
    assert_eq!(notify.asked(&[], true, false, Lang::En), None);
    notify.focus(false, false);
    assert_eq!(
        notify.asked(&[], true, false, Lang::En),
        Some("ZCode has news for you")
    );
    assert_eq!(
        notify.title_bytes("Fix"),
        Some("\x1b[22;0t\x1b]0;● Fix\x07".to_owned())
    );
    // 没有请求开着，但通知还没看到：标题留着，直到回到终端。
    assert_eq!(notify.asked(&[], false, false, Lang::En), None);
    assert_eq!(notify.title_bytes("Fix"), None);
    notify.focus(true, false);
    assert_eq!(notify.title_bytes("Fix"), Some("\x1b]0;Fix\x07".to_owned()));
    // 换了会话：那个会话已有的行只记下。
    assert!(!notify.fresh_rows(Some("s2"), &rows));
}

#[test]
fn what_rings() {
    use rust_cli_kernel_api::goal::GoalStatus;
    let goal = |met, failed| {
        Body::Goal(Box::new(GoalStatus {
            met,
            sentinel: false,
            failed,
            condition: "tests pass".into(),
            reason: None,
            iterations: None,
            duration_ms: None,
            tokens: None,
        }))
    };
    assert!(rings(&goal(true, false)));
    assert!(rings(&goal(false, true)));
    assert!(!rings(&goal(false, false)), "not yet");
    assert!(rings(&peer("work-b")));
    assert!(!rings(&Body::Output("hi".into())));
}
