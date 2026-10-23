//! 请求头的名字与常用的值：发请求的各处（model、platform、env-local、cli）取这里，不各写一份
//! （docs/specs/rust-cli/README.md §4.1，工作线 F55）。
//!
//! 名字用小写：发送时按 `http::HeaderName` 规范化成小写（reqwest 没开 title case），写法不影响线上的字节。只在内核
//! crate 里组装的请求（hook 的 HTTP、WebFetch）不能依赖 net，那里也写小写（F58，README §4.1）。

use rust_cli_base::secret::RedactedString;

pub const ACCEPT: &str = "accept";
pub const AUTHORIZATION: &str = "authorization";
pub const CONTENT_TYPE: &str = "content-type";
pub const RETRY_AFTER: &str = "retry-after";
pub const USER_AGENT: &str = "user-agent";

/// `application/json`。
pub const JSON: &str = "application/json";
/// `text/event-stream`（SSE）。
pub const EVENT_STREAM: &str = "text/event-stream";

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
