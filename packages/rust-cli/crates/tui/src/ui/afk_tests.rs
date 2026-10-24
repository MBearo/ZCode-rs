use rust_cli_kernel_api::ids::{InteractionId, ToolUseId};
use rust_cli_kernel_api::orchestration::InteractionRequest;
use serde_json::json;
use tokio::time::{Duration, Instant};

use super::{Afk, QuestionTimeout, Tick};

fn question() -> InteractionRequest {
    InteractionRequest::Question {
        tool_use: ToolUseId::new("tu1"),
        questions: json!([]),
    }
}

fn afk(timeout_ms: u64, countdown_ms: Option<u64>) -> Afk {
    Afk::new(QuestionTimeout::new(None, Some(timeout_ms), countdown_ms))
}

#[test]
fn the_environment_wins_over_the_setting_and_the_threshold_is_capped() {
    assert_eq!(QuestionTimeout::new(None, None, Some(5_000)), None);
    let both = QuestionTimeout::new(Some(60_000), Some(4_000), None).unwrap();
    assert_eq!((both.timeout_ms, both.countdown_ms), (4_000, 4_000));
    let setting = QuestionTimeout::new(Some(300_000), None, Some(30_000)).unwrap();
    assert_eq!(
        (setting.timeout_ms, setting.countdown_ms),
        (300_000, 30_000)
    );
    let default = QuestionTimeout::new(Some(60_000), None, None).unwrap();
    assert_eq!(default.countdown_ms, 20_000);
}

/// 门槛等于超时：弹出就显示倒计时，每秒减一，到期提交。
#[test]
fn a_short_timeout_counts_down_from_the_start_and_expires() {
    let start = Instant::now();
    let mut afk = afk(4_000, None);
    let id = InteractionId::new("i1");
    afk.dialog(Some((&id, &question())), start);
    assert_eq!(afk.countdown(), Some(4));
    assert_eq!(afk.next_due(start), Some(start + Duration::from_secs(1)));
    let at = |seconds| start + Duration::from_secs(seconds);
    assert_eq!(afk.tick(at(1)), Some(Tick::Waiting(Some(3))));
    assert_eq!(afk.tick(at(3)), Some(Tick::Waiting(Some(1))));
    assert_eq!(afk.tick(at(4)), Some(Tick::Expired(4_000)));
    assert_eq!(afk.countdown(), None);
    // 到期之后不再检查。
    assert_eq!(afk.next_due(at(4)), None);
    assert_eq!(afk.tick(at(5)), None);
}

/// 门槛以前不显示；任何输入重新计时（倒计时消失）。
#[test]
fn input_restarts_the_countdown() {
    let start = Instant::now();
    let mut afk = afk(60_000, None);
    let id = InteractionId::new("i1");
    afk.dialog(Some((&id, &question())), start);
    assert_eq!(afk.countdown(), None);
    let at = |seconds| start + Duration::from_secs(seconds);
    assert_eq!(afk.tick(at(10)), Some(Tick::Waiting(None)));
    assert_eq!(afk.tick(at(41)), Some(Tick::Waiting(Some(19))));
    assert_eq!(afk.countdown(), Some(19));
    afk.input(at(41));
    assert_eq!(afk.tick(at(42)), Some(Tick::Waiting(None)));
    assert_eq!(afk.tick(at(100)), Some(Tick::Waiting(Some(1))));
    assert_eq!(afk.tick(at(101)), Some(Tick::Expired(60_000)));
}

/// 终端报告有焦点时不计；失去焦点后从那时起计。
#[test]
fn a_focused_terminal_does_not_count() {
    let start = Instant::now();
    let mut afk = afk(4_000, None);
    let id = InteractionId::new("i1");
    afk.dialog(Some((&id, &question())), start);
    afk.focus(true);
    let at = |seconds| start + Duration::from_secs(seconds);
    assert_eq!(afk.tick(at(10)), Some(Tick::Waiting(Some(4))));
    afk.focus(false);
    assert_eq!(afk.tick(at(13)), Some(Tick::Waiting(Some(1))));
    assert_eq!(afk.tick(at(14)), Some(Tick::Expired(4_000)));
}

/// 不是提问、没有配置时不计时；换了提问重新计时。
#[test]
fn only_questions_are_timed() {
    let start = Instant::now();
    let id = InteractionId::new("i1");
    let mut off = Afk::new(None);
    off.dialog(Some((&id, &question())), start);
    assert_eq!(off.next_due(start), None);
    let mut afk = afk(4_000, None);
    let permission = InteractionRequest::Permission {
        machine: None,
        tool_use: ToolUseId::new("tu1"),
        tool: "Bash".to_owned(),
        input: json!({}),
        message: String::new(),
        suggestions: Vec::new(),
        asked_by: None,
    };
    afk.dialog(Some((&id, &permission)), start);
    assert_eq!(afk.next_due(start), None);
    afk.dialog(Some((&id, &question())), start);
    let later = start + Duration::from_secs(3);
    let next = InteractionId::new("i2");
    afk.dialog(Some((&next, &question())), later);
    assert_eq!(
        afk.tick(later + Duration::from_secs(1)),
        Some(Tick::Waiting(Some(3)))
    );
    afk.dialog(None, later);
    assert_eq!(afk.next_due(later), None);
}
