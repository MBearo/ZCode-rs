//! 重试行的话与倒计时（tui-kit.md §4.0「尾巴」）。
use rust_cli_kernel_api::model::{ModelError, ModelNotice};

use super::*;

fn retrying(attempt: u32, error: ModelError) -> Notice {
    Notice {
        notice: ModelNotice::Retrying {
            attempt,
            max_retries: 10,
            delay_ms: 5_000,
            error,
        },
        at_ms: 1_000,
    }
}

/// 第 `attempt` 次失败、还剩 4 秒时的整句（原因、倒计时、次数用 `  ·  ` 接起来，同 kit 的行）。
fn said(attempt: u32, error: ModelError, lang: Lang) -> String {
    let (reason, wait, count) = tail_words(&retrying(attempt, error), 2_000, lang).unwrap();
    [Some(reason), Some(wait), count]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("  ·  ")
}

fn network(message: &str) -> ModelError {
    ModelError::Network {
        message: message.to_owned(),
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

/// 修 bug（2026-11-02）：以前 5xx 只说 `The provider had a problem`；现在接状态码，网关的 HTML 正文不上屏。
#[test]
fn server_errors_name_the_status_not_the_page() {
    let html = ModelError::Server {
        status: 502,
        message: "<html>\r\n<head><title>502 Bad Gateway</title></head>\r\n</html>".to_owned(),
    };
    assert_eq!(
        said(1, html.clone(), Lang::En),
        "The provider had a problem (HTTP 502)  ·  trying again in 4s"
    );
    assert_eq!(
        said(3, html, Lang::Zh),
        "服务商出了问题（HTTP 502）  ·  4 秒后重试  ·  已试 3 次"
    );
}

/// 网络错误写错误链的根因（最后一段）：整句的前面是请求的包装，窄屏上根因会被截掉（2026-11-02 实测）。
#[test]
fn network_errors_name_the_root_cause() {
    assert_eq!(
        said(2, network("request timed out"), Lang::En),
        "Could not reach the provider (request timed out)  ·  trying again in 4s  ·  tried 2 times"
    );
    assert_eq!(
        said(1, network("TLS error: tls handshake eof"), Lang::En),
        "Could not reach the provider (tls handshake eof)  ·  trying again in 4s"
    );
    let chain = "network error: error sending request for url (http://127.0.0.1:47812/v1/messages?beta=true): \
client error (SendRequest): connection closed before message completed";
    assert_eq!(
        said(1, network(chain), Lang::Zh),
        "连不上服务商（connection closed before message completed）  ·  4 秒后重试"
    );
}

#[test]
fn overload_has_its_own_words() {
    let busy = ModelError::Overloaded {
        message: "Overloaded".to_owned(),
    };
    assert_eq!(
        said(1, busy.clone(), Lang::En),
        "The provider is overloaded  ·  trying again in 4s"
    );
    assert_eq!(said(1, busy, Lang::Zh), "服务商过载  ·  4 秒后重试");
}

#[test]
fn kinds_that_say_enough_get_no_detail() {
    let limited = ModelError::RateLimited {
        message: "slow down".to_owned(),
        retry_after_ms: None,
    };
    assert_eq!(
        said(1, limited, Lang::En),
        "The provider is limiting requests  ·  trying again in 4s"
    );
    let dropped = ModelError::Protocol {
        message: "stream ended".to_owned(),
    };
    assert_eq!(
        said(1, dropped, Lang::En),
        "The connection dropped  ·  trying again in 4s"
    );
}

#[test]
fn other_errors_add_their_first_line() {
    let odd = ModelError::InvalidRequest {
        message: "Request Timeout\nmore lines".to_owned(),
    };
    assert_eq!(
        said(1, odd.clone(), Lang::En),
        "The provider had a problem: invalid request: Request Timeout  ·  trying again in 4s"
    );
    assert_eq!(
        said(1, odd, Lang::Zh),
        "服务商出了问题：invalid request: Request Timeout  ·  4 秒后重试"
    );
}

#[test]
fn stalls_have_no_detail_and_no_count() {
    let stalled = Notice {
        notice: ModelNotice::Stalled { retry_in_ms: 0 },
        at_ms: 1_000,
    };
    assert_eq!(
        tail_words(&stalled, 2_000, Lang::En),
        Some((
            "The answer stopped coming".to_owned(),
            "trying again now".to_owned(),
            None
        ))
    );
    let resumed = Notice {
        notice: ModelNotice::Resumed,
        at_ms: 1_000,
    };
    assert_eq!(tail_words(&resumed, 2_000, Lang::En), None);
}
