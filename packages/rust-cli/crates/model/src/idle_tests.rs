//! 看门狗的上限与醒来时刻（stall-visibility.md §2.2、§2.4）。
use super::*;

fn limits(events: u64, bytes: u64) -> Limits {
    StreamIdle {
        events: Some(Duration::from_secs(events)),
        bytes: Some(ByteIdle::Fixed(Duration::from_secs(bytes))),
        stall_notice: STALL_NOTICE,
    }
    .limits(Duration::from_secs(300))
}

#[test]
fn the_default_byte_limit_depends_on_the_endpoint() {
    use crate::profiles::select;
    use rust_cli_kernel_api::provider::WireProtocol;
    let idle = StreamIdle::default();
    let profile = |base_url: &str, model: &str| {
        select(WireProtocol::AnthropicMessages, base_url, model).byte_idle
    };
    let official = idle.limits(profile("https://api.anthropic.com", "claude-opus-5-5"));
    let other = idle.limits(profile("http://127.0.0.1:9", "claude-opus-5-5"));
    // 智谱、DeepSeek 的官方端点不算第一方：只有 Anthropic 官方端点是第一方。
    let zhipu = idle.limits(profile("https://api.z.ai/api/anthropic", "glm-5.1"));
    assert_eq!(zhipu.bytes, Some(Duration::from_secs(300)));
    assert_eq!(official.bytes, Some(Duration::from_secs(180)));
    assert_eq!(other.bytes, Some(Duration::from_secs(300)));
    assert_eq!(official.events, Some(Duration::from_secs(300)));
    assert_eq!(official.stall, Duration::from_secs(20));
}

#[test]
fn the_stall_notice_comes_first_then_the_nearest_deadline() {
    let start = Instant::now();
    let mut watch = Watch::new(limits(300, 60), start);
    assert_eq!(
        watch.next(),
        Some((start + Duration::from_secs(20), Wake::Stall))
    );
    // 停顿通知之后离字节级上限还有 40 s。
    let at = start + Duration::from_secs(20);
    assert_eq!(watch.stall(at), Duration::from_secs(40));
    assert_eq!(
        watch.next(),
        Some((
            start + Duration::from_secs(60),
            Wake::Idle(Duration::from_secs(60))
        ))
    );
    // 字节重新计时字节级；事件级还按上一个事件算。
    watch.bytes(at);
    assert_eq!(
        watch.next(),
        Some((
            at + Duration::from_secs(60),
            Wake::Idle(Duration::from_secs(60))
        ))
    );
    // 收到事件：告诉调用方先交付 Resumed，停顿通知重新可用。
    assert!(watch.event(false, at));
    assert!(!watch.event(false, at));
    assert_eq!(
        watch.next(),
        Some((at + Duration::from_secs(20), Wake::Stall))
    );
}

#[test]
fn the_event_limit_wins_when_it_is_nearer() {
    let start = Instant::now();
    let mut watch = Watch::new(limits(30, 300), start);
    watch.stall(start);
    // 字节一直在来，但没有事件：事件级上限先到。
    watch.bytes(start + Duration::from_secs(25));
    assert_eq!(
        watch.next(),
        Some((
            start + Duration::from_secs(30),
            Wake::Idle(Duration::from_secs(30))
        ))
    );
}

#[test]
fn only_the_first_thirty_pings_in_a_row_count() {
    let start = Instant::now();
    let mut watch = Watch::new(limits(300, 1800), start);
    let late = start + Duration::from_secs(100);
    for _ in 0..30 {
        watch.event(true, start);
    }
    watch.stall(start);
    // 第 31 个 ping 不重新计时，也不清掉停顿。
    assert!(!watch.event(true, late));
    assert_eq!(
        watch.next(),
        Some((
            start + Duration::from_secs(300),
            Wake::Idle(Duration::from_secs(300))
        ))
    );
    // 普通事件重新计数。
    assert!(watch.event(false, late));
    assert!(!watch.event(true, late));
}

#[test]
fn no_watchdog_means_no_wake() {
    let idle = StreamIdle {
        events: None,
        bytes: None,
        stall_notice: STALL_NOTICE,
    };
    let watch = Watch::new(idle.limits(Duration::from_secs(300)), Instant::now());
    assert_eq!(watch.next(), None);
}
