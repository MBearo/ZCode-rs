// 前五个用例复制自 Codex `codex-rs/tui/src/bottom_pane/paste_burst.rs` 的测试（c3d3b142d，Apache License 2.0，
// Copyright 2025 OpenAI），改写成本工程的写法；`deadline` 的用例是新加的。
use std::time::{Duration, Instant};

use super::*;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// 越过字符间隔的延迟。
const FLUSH: Duration = PASTE_BURST_CHAR_INTERVAL.saturating_add(Duration::from_millis(1));
/// 越过空闲超时的延迟。
const ACTIVE_FLUSH: Duration =
    PASTE_BURST_ACTIVE_IDLE_TIMEOUT.saturating_add(Duration::from_millis(1));

/// ASCII 的第一个快速字符先扣住；后面没有 burst 时作为普通键入交出。
#[test]
fn ascii_first_char_is_held_then_flushes_as_typed() {
    let mut burst = PasteBurst::default();
    let t0 = Instant::now();
    assert_eq!(burst.on_plain_char('a', t0), CharDecision::RetainFirstChar);
    assert_eq!(
        burst.flush_if_due(t0 + FLUSH + ms(1)),
        FlushResult::Typed('a')
    );
    assert!(!burst.is_active());
}

/// 两个 ASCII 字符很快到达：不画第一个，直接开始缓存，最后整段作为一次粘贴交出。
#[test]
fn ascii_two_fast_chars_start_buffer_from_pending_and_flush_as_paste() {
    let mut burst = PasteBurst::default();
    let t0 = Instant::now();
    assert_eq!(burst.on_plain_char('a', t0), CharDecision::RetainFirstChar);
    let t1 = t0 + ms(1);
    assert_eq!(
        burst.on_plain_char('b', t1),
        CharDecision::BeginBufferFromPending
    );
    burst.append_char_to_buffer('b', t1);
    assert_eq!(
        burst.flush_if_due(t1 + ACTIVE_FLUSH + ms(1)),
        FlushResult::Paste("ab".to_owned())
    );
}

/// 别的输入之前立即交出（含扣住的字符），状态不留到下一个输入。
#[test]
fn flush_before_modified_input_includes_pending_first_char() {
    let mut burst = PasteBurst::default();
    let t0 = Instant::now();
    assert_eq!(burst.on_plain_char('a', t0), CharDecision::RetainFirstChar);
    assert_eq!(burst.flush_before_modified_input(), Some("a".to_owned()));
    assert!(!burst.is_active());
}

/// 收回已键入的前缀只在它像粘贴时（含空白或够长）：短的输入法上屏不误判。
#[test]
fn decide_begin_buffer_only_triggers_for_pastey_prefixes() {
    let mut burst = PasteBurst::default();
    let now = Instant::now();
    assert!(burst.decide_begin_buffer(now, "ab", 2).is_none());
    assert!(!burst.is_active());
    let grab = burst.decide_begin_buffer(now, "a b", 2).unwrap();
    assert_eq!(grab.start_byte, 1);
    assert_eq!(grab.grabbed, " b");
    assert!(burst.is_active());
    // 按字符数收回，多字节的字符不切开。
    assert_eq!(retro_start_index("第一行", 2), "第".len());
}

/// burst 交出之后还有一小段时间，稍晚的 enter 仍然插入换行而不发送。
#[test]
fn newline_suppression_window_outlives_buffer_flush() {
    let mut burst = PasteBurst::default();
    let t0 = Instant::now();
    assert_eq!(burst.on_plain_char('a', t0), CharDecision::RetainFirstChar);
    let t1 = t0 + ms(1);
    assert_eq!(
        burst.on_plain_char('b', t1),
        CharDecision::BeginBufferFromPending
    );
    burst.append_char_to_buffer('b', t1);
    let t2 = t1 + ACTIVE_FLUSH + ms(1);
    assert_eq!(burst.flush_if_due(t2), FlushResult::Paste("ab".to_owned()));
    assert!(!burst.is_active());
    assert!(burst.newline_should_insert_instead_of_submit(t2));
    let t3 = t1 + PASTE_ENTER_SUPPRESS_WINDOW + ms(1);
    assert!(!burst.newline_should_insert_instead_of_submit(t3));
}

/// 事件循环的计时：扣着字符时在 8 ms 之后，缓存中在最后一个字符 60 ms 之后，都多 1 ms；空闲时没有。
#[test]
fn the_deadline_follows_the_two_timeouts() {
    let mut burst = PasteBurst::default();
    assert_eq!(burst.deadline(), None);
    let t0 = Instant::now();
    burst.on_plain_char('a', t0);
    assert_eq!(burst.deadline(), Some(t0 + ms(9)));
    let t1 = t0 + ms(2);
    burst.on_plain_char('b', t1);
    burst.append_char_to_buffer('b', t1);
    assert_eq!(burst.deadline(), Some(t1 + ms(61)));
    assert!(burst.append_control_char_if_active('\n', t1 + ms(3)));
    assert_eq!(burst.deadline(), Some(t1 + ms(64)));
    assert_eq!(burst.flush_if_due(t1 + ms(63)), FlushResult::None);
    assert_eq!(
        burst.flush_if_due(t1 + ms(64)),
        FlushResult::Paste("ab\n".to_owned())
    );
    assert_eq!(burst.deadline(), None);
}
