//! 第 2 层：处置表的 P0、P1 两列（docs/specs/rust-cli/provider-errors.md §2.4，修复线 F113）。
//!
//! 纯函数：只按中性分类、阶段与来源给出动作。计数与上限由执行点（transport 的 `Connect`）按动作去做，每个计数只有它
//! 一个 owner（§4）。P2t、P2 与结束原因由 query 按 `ModelError` / `StopReason` 判断（同一张表的后两列）。

use super::Class;

/// 失败发生在哪个阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    /// P0：拿到响应头之前（HTTP 非 2xx、传输错误）。
    Head,
    /// P1：流已开始，还没越过交出边界（调用方一个内容事件都还没收到）。
    Stream,
}

/// transport 对一次失败做什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    /// 退避后重发（`attempt` 计数）；次数用尽时按 `exhausted`。
    Retry { exhausted: Exhausted },
    /// 过载：计入过载次数；第 3 次有 fallback 时换模型，否则 P0 按模型系列、P1 走 G（`Connect` 做）。
    Overload,
    /// 账号或套餐的并发上限：最多重试 2 次（D5 暂定）。
    Capacity,
    /// 截断：重试 1 次，之后走 G（D17 暂定）。
    Truncation,
    /// G：用一次流式重发代替非流式重发（D3 暂定 (b)）。
    Resend,
    /// 不重试，交给调用方（终止、压缩都由 query 按错误做）。
    Fail,
}

/// 重试次数用尽之后。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Exhausted {
    Fail,
    Resend,
}

const RETRY: Action = Action::Retry {
    exhausted: Exhausted::Fail,
};

/// 一次失败的动作。`foreground`：用户在等结果的来源（过载时才重试）；`should_retry`：响应头里的 `x-should-retry`
/// （只在 P0 有）。过载先认、不看响应头；其余分类先看这个头，再按表（model.md §5.2 的顺序，现状）。
pub(crate) fn dispose(
    class: Class,
    phase: Phase,
    foreground: bool,
    should_retry: Option<bool>,
) -> Action {
    let header = if phase == Phase::Head {
        should_retry
    } else {
        None
    };
    match (class, phase) {
        (Class::Overloaded, _) if foreground => Action::Overload,
        (Class::Overloaded, _) => Action::Fail,
        _ if header == Some(true) => RETRY,
        _ if header == Some(false) => Action::Fail,
        (
            Class::RateLimited | Class::ServerError | Class::Network | Class::AuthRejected,
            Phase::Head,
        )
        | (Class::RateLimited, Phase::Stream) => RETRY,
        (Class::CapacityLimited, _) => Action::Capacity,
        (Class::ServerError | Class::Malformed, Phase::Stream) => Action::Resend,
        (Class::Network, Phase::Stream) => Action::Retry {
            exhausted: Exhausted::Resend,
        },
        (Class::Truncated, Phase::Stream) => Action::Truncation,
        // 流里来的参数错误不走 G（D15 暂定）：同一份请求体再流式发一次改不了它。
        (
            Class::TlsFailure
            | Class::AuthRejected
            | Class::AccessDenied
            | Class::QuotaExhausted
            | Class::ContentBlocked
            | Class::ContextOverflow
            | Class::TooLarge
            | Class::InvalidRequest
            | Class::Malformed
            | Class::Truncated,
            _,
        ) => Action::Fail,
    }
}

#[cfg(test)]
#[path = "dispose_tests.rs"]
mod tests;
