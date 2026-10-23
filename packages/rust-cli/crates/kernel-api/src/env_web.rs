//! 执行环境端口里取网页的 DTO（WebFetch，docs/specs/rust-cli/tools-more.md §4）。
//!
//! 端口只做一次 GET，不跟随重定向：同主机的重定向跟随、跨主机的交还模型，这些判断属于工具语义，
//! 在内核里做（§6 规则 2）。网络出口在本地实现里只经 rust-cli-net。

use serde::{Deserialize, Serialize};

/// 一次 GET。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchUrl {
    pub url: String,
    /// 本请求的 header（名、值）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<(String, String)>,
    /// 整个请求（含读正文）的时限。
    pub timeout_ms: u64,
    /// 正文最多读这么多字节；超过时报错，不截断。
    pub max_bytes: u64,
}

/// 响应：任何状态码都是 `Ok`，由工具分类。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchedUrl {
    pub status: u16,
    /// `content-type`；没有时为空串。
    #[serde(default)]
    pub content_type: String,
    /// 重定向的 `location`（原样，可能是相对地址）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub body: Vec<u8>,
}
