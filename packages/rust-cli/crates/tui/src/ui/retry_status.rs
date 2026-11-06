//! 重试行要说的话（docs/specs/rust-cli/tui-kit.md §4.0「尾巴」）与倒计时的写法。行的样子在 kit（`frame::retry_line_with`）。
//!
//! 修 bug（2026-11-02，用户反馈）：以前原因只按错误的种类写（Puffin `retry_words`），`The provider had a problem` 后面
//! 什么都没有，日志之外看不出是 502、超时还是别的，没法排查。以前这里还留着旧状态行（`API error · Retrying in 5s ·
//! attempt 1/10`）的整套代码，尾巴接管重试行之后已没有调用方，一起删掉。现在原因后接一项事实（[`detail`]）。
use rust_cli_base::units;
use rust_cli_kernel_api::model::{ModelError, ModelNotice};

use super::cells::duration;
use super::i18n::Lang;
use crate::view::Notice;

/// 倒计时从这么长起只写最大的单位。
const LARGEST_UNIT_FROM_MS: u64 = 5 * units::MS_PER_MINUTE;

/// 倒计时：向上取整到秒；不少于 5 分钟时只写最大的单位（这里已是整秒，秒不会进位）。
pub fn countdown(ms: u64) -> String {
    let ms = ms.div_ceil(units::MS_PER_SECOND) * units::MS_PER_SECOND;
    if ms < LARGEST_UNIT_FROM_MS {
        return duration(ms);
    }
    super::quantities::largest_unit_text(ms)
}

/// 尾巴里的重试行要说的（Puffin `retry_line` 的参数）：原因、倒计时与失败的次数的说法。原因照 Puffin `retry_words` 按错误的
/// 种类写（Puffin 宿主的 `retry_code`），后面接 [`detail`] 的事实；旧界面没有模型目录，provider 的名字用 Puffin 找不到目录项时
/// 的说法（`The provider`）。停顿（等不到回答）是 `The answer stopped coming`，没有次数。
pub fn tail_words(
    notice: &Notice,
    now_ms: u64,
    lang: Lang,
) -> Option<(String, String, Option<String>)> {
    let secs = notice.remaining_ms(now_ms).div_ceil(units::MS_PER_SECOND);
    let zh = lang == Lang::Zh;
    let (code, attempt, error) = match &notice.notice {
        ModelNotice::Retrying { attempt, error, .. } => (retry_code(error), *attempt, Some(error)),
        ModelNotice::Stalled { .. } => ("fault.network.sseStalled", 0, None),
        ModelNotice::Resumed => return None,
    };
    let sentence = if zh {
        match code {
            "fault.provider.rateLimited" => "服务商在限流".to_owned(),
            "fault.provider.overloaded" => "服务商过载".to_owned(),
            "fault.network.unreachable" => "连不上服务商".to_owned(),
            "fault.network.sseDisconnected" => "连接断了".to_owned(),
            "fault.network.sseStalled" => "回答停住了".to_owned(),
            _ => "服务商出了问题".to_owned(),
        }
    } else {
        rust_cli_tui_kit::app::frame::retry_words(code, "The provider", "the provider")
    };
    let reason = match error.and_then(detail) {
        Some(Detail::Fact(fact)) if zh => format!("{sentence}（{fact}）"),
        Some(Detail::Fact(fact)) => format!("{sentence} ({fact})"),
        Some(Detail::Said(said)) if zh => format!("{sentence}：{said}"),
        Some(Detail::Said(said)) => format!("{sentence}: {said}"),
        None => sentence,
    };
    let wait = match (zh, secs) {
        (false, 0) => "trying again now".to_owned(),
        (false, s) => format!("trying again in {s}s"),
        (true, 0) => "正在重试".to_owned(),
        (true, s) => format!("{s} 秒后重试"),
    };
    let count = (attempt >= 2).then(|| {
        if zh {
            format!("已试 {attempt} 次")
        } else {
            format!("tried {attempt} times")
        }
    });
    Some((reason, wait, count))
}

/// 原因后面接的事实。
#[derive(Debug, PartialEq)]
enum Detail {
    /// 短的事实，写在括号里：`HTTP 502`、`request timed out`。
    Fact(String),
    /// 端点或传输层的原话（第一行），写在冒号后面。
    Said(String),
}

/// 原因后面接什么：5xx 只写状态码（正文常是网关的整页 HTML，2026-11-01 日志里的 502 就是）；网络错误写传输层错误链的
/// 最后一段，即根因（链用 `: ` 接起来，前面是 `error sending request for url (…)` 这类包装，整句一长根因就被截掉了）：
/// `connection closed before message completed`、`request timed out`、`tls handshake eof`；种类已经说清的（限流、过载、
/// 连接断了、停住）不接；其余写错误的第一行。窄时由 kit 用 `…` 截原因，倒计时保留。
fn detail(error: &ModelError) -> Option<Detail> {
    let first_line = |text: &str| {
        let line = text.lines().next().unwrap_or_default().trim();
        (!line.is_empty()).then(|| line.to_owned())
    };
    match error {
        ModelError::Server { status, .. } => Some(Detail::Fact(format!("HTTP {status}"))),
        ModelError::Network { message } => {
            let line = message.lines().next().unwrap_or_default();
            first_line(line.rsplit(": ").next().unwrap_or(line)).map(Detail::Fact)
        }
        ModelError::RateLimited { .. }
        | ModelError::Overloaded { .. }
        | ModelError::Protocol { .. }
        | ModelError::StreamIdle { .. } => None,
        other => first_line(&other.to_string()).map(Detail::Said),
    }
}

/// 重试的 reasonCode（照 Puffin 宿主 `host/fold/turns.rs` 的 `retry_code`）。
fn retry_code(error: &ModelError) -> &'static str {
    match error {
        ModelError::RateLimited { .. } => "fault.provider.rateLimited",
        ModelError::Network { .. } => "fault.network.unreachable",
        ModelError::StreamIdle { .. } => "fault.network.sseStalled",
        ModelError::Protocol { .. } => "fault.network.sseDisconnected",
        ModelError::Overloaded { .. } => "fault.provider.overloaded",
        ModelError::Server { .. } => "fault.provider.serverError",
        _ => "fault.provider.error",
    }
}

#[cfg(test)]
#[path = "retry_status_tests.rs"]
mod tests;
