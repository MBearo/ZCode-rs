//! 重试与停顿状态行的文字（stall-visibility.md §3）。
use rust_cli_kernel_api::model::{ModelError, ModelNotice};

use super::*;

fn text(spans: &[Span<'_>]) -> String {
    spans.iter().map(|span| span.content.as_ref()).collect()
}

fn retrying(attempt: u32, delay_ms: u64) -> Notice {
    Notice {
        notice: ModelNotice::Retrying {
            attempt,
            max_retries: 10,
            delay_ms,
            error: ModelError::Server {
                status: 529,
                message: "Overloaded".to_owned(),
            },
        },
        at_ms: 1_000,
    }
}

#[test]
fn countdowns_round_up_and_keep_the_largest_unit_from_five_minutes() {
    assert_eq!(countdown(0), "0s");
    assert_eq!(countdown(1), "1s");
    assert_eq!(countdown(4_200), "5s");
    assert_eq!(countdown(65_000), "1m 5s");
    assert_eq!(countdown(280_000), "4m 40s");
    assert_eq!(countdown(300_000), "5m");
    assert_eq!(countdown(359_000), "5m");
    assert_eq!(countdown(7_300_000), "2h");
    assert_eq!(countdown(90_000_000), "1d");
}

#[test]
fn retries_say_api_error_first_and_the_error_from_the_third_attempt() {
    let first = spans(&retrying(1, 5_000), 2_000, Lang::En, 120);
    assert_eq!(text(&first), "✻ API error · Retrying in 4s · attempt 1/10");
    assert_eq!(first[0].style.fg, Some(Color::Red));
    assert_eq!(first[1].style.fg, Some(Color::Red));
    assert!(first[2].style.add_modifier.contains(Modifier::DIM));
    let third = spans(&retrying(3, 5_000), 1_000, Lang::En, 120);
    assert_eq!(
        text(&third),
        "✻ server error 529: Overloaded · Retrying in 5s · attempt 3/10"
    );
    let zh = spans(&retrying(1, 5_000), 1_000, Lang::Zh, 120);
    assert_eq!(text(&zh), "✻ API 错误 · 5s后重试 · 第 1/10 次");
}

#[test]
fn stalls_count_down_to_the_watchdog() {
    let stalled = Notice {
        notice: ModelNotice::Stalled {
            retry_in_ms: 280_000,
        },
        at_ms: 0,
    };
    assert_eq!(
        text(&spans(&stalled, 0, Lang::En, 120)),
        "✻ Waiting for API response · will retry in 4m 40s · check your network"
    );
    assert_eq!(
        text(&spans(&stalled, 0, Lang::Zh, 120)),
        "✻ 等待 API 响应 · 4m 40s后重试 · 请检查网络"
    );
}

#[test]
fn the_error_is_cut_so_the_countdown_fits() {
    let long = Notice {
        notice: ModelNotice::Retrying {
            attempt: 5,
            max_retries: 10,
            delay_ms: 2_000,
            error: ModelError::Network {
                message: "connection reset by peer while reading the response body".to_owned(),
            },
        },
        at_ms: 0,
    };
    let line = text(&spans(&long, 0, Lang::En, 60));
    assert!(line.ends_with(" · Retrying in 2s · attempt 5/10"), "{line}");
    assert!(line.contains('…'), "{line}");
}
