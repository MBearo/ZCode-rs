//! 网络出口的唯一入口（docs/specs/rust-cli/README.md §4，细则见 docs/specs/rust-cli/model.md §2）。
//!
//! 模型请求、WebFetch、MCP HTTP、插件下载、OAuth、账号 API 都经这里发出：
//!
//! - [`Egress`]：持有出口策略 [`NetworkPolicy`] 与按它构造的 HTTP 客户端。策略整份热替换
//!   （R4 快照），每个请求开始时读一次快照；以后要给所有请求加 header，只改策略。代理、不走代理的规则
//!   （[`NoProxy`]）与根证书（[`TrustRoots`]）由入口按环境变量算出（docs/specs/rust-cli/config.md §2.4），
//!   net 不读进程环境。
//! - [`HttpRequest`] / [`HttpResponse`]：发送请求、按块读取响应字节流。上层不接触 reqwest。
//! - [`sse`]：SSE 解码（复制自 `apps/zcode-cli-rust/crates/model/src/sse.rs`）。
//! - [`uds`]：本机的 Unix 域套接字（跨会话的收件箱，docs/specs/rust-cli/cross-session.md §2.1）。
//! - [`retry`]：与协议无关的退避策略、Retry-After 与 `x-should-retry` 解析。哪些错误可以重试由
//!   协议适配器决定（例如 Anthropic 的 `overloaded_error`）。
//! - [`timeout`]：每种请求的单次超时（只等响应头），按服务各有来源，只定义在这里（F58）。
//!
//! 审计日志只记方法、脱敏后的 URL、状态码与耗时；header 值不进日志，错误文本按值脱敏
//! （`rust_cli_base::redact`）。

mod egress;
mod error;
pub mod header;
mod http;
mod no_proxy;
pub mod retry;
mod roots;
pub mod sse;
pub mod timeout;
pub mod uds;

pub use bytes::Bytes;
pub use egress::{Egress, NetworkPolicy};
pub use error::NetError;
pub use http::{HeaderValue, HttpRequest, HttpResponse, Method, ResponseHeaders};
pub use no_proxy::NoProxy;
pub use roots::{CaBundle, TrustRoots};
