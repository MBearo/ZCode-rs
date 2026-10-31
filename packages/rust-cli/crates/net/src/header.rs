//! 请求头的名字与常用的值：发请求的各处（model、platform、env-local、cli）取这里，不各写一份
//! （docs/specs/rust-cli/README.md §4.1，工作线 F55）。
//!
//! 名字用小写：发送时按 `http::HeaderName` 规范化成小写（reqwest 没开 title case），写法不影响线上的字节。只在内核
//! crate 里组装的请求（hook 的 HTTP、WebFetch）不能依赖 net，那里直接用 base 的 `proto`（F122）。

use rust_cli_base::secret::RedactedString;

// 通用的头名与 MIME 定义在 base 的 `proto`（F122：hooks、tools 不依赖 net，也要用同一份），这里重新导出，调用方不变。
pub use rust_cli_base::proto::http::header::{
    ACCEPT, AUTHORIZATION, CONTENT_TYPE, RETRY_AFTER, USER_AGENT,
};
pub use rust_cli_base::proto::mime::{EVENT_STREAM, JSON};

/// Anthropic 协议的 API key 头（model 发、platform 与模型轨迹按秘密值处理）。只定义在这里（工作线 F122）。
pub const X_API_KEY: &str = "x-api-key";

/// 模型请求的 User-Agent：`ZCode/<版本>`（修复线 F58，照 Node `buildCliZCodeSourceHeaders`，模型端点是与 Node 共用的
/// 后端；ai-sdk 追加的后缀不冒用）。账号接口与 OAuth 不发（Node 不设）。
pub fn zcode_user_agent(version: &str) -> String {
    format!("ZCode/{version}")
}

/// `Bearer <token>`，作为秘密值（Debug 不含原值）。
pub fn bearer(token: &str) -> RedactedString {
    RedactedString::new(format!("Bearer {token}"))
}

#[cfg(test)]
#[path = "header_tests.rs"]
mod tests;
