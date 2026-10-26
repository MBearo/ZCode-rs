//! 账号模型每请求鉴权的外部来源（缺口 G8，docs/specs/rust-cli/app-protocol.md §5.2、platform.md §5.3）。
//!
//! 平台层缺省自己算账号凭据（手工 key，或 OAuth access token 换来的项目令牌）。组装根可以注入一个来源：之后账号
//! 模型的凭据与请求头都向它要（例如由宿主按它的账号状态给出）；API key 的 provider 不受影响。来源的形状与前端无关：
//! 只认模型选择、账号的类别与模式、发请求的会话与被拒的凭据。

use async_trait::async_trait;
use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::model::{ModelError, ModelSelection};

/// 向来源要凭据的一次请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderRequest {
    /// 这次模型请求的选择。
    pub selection: ModelSelection,
    /// 账号的类别（Provider 配置的 `access.accountType`）。
    pub account_type: String,
    /// 账号的模式（Provider 配置的 `access.mode`）。
    pub mode: String,
    /// 发请求的会话（[`rust_cli_kernel_api::provider::EndpointContext::session`]）；没有时来源可以拒绝。
    pub session: Option<SessionId>,
    /// 刚被模型服务拒绝（401）的凭据的 sha256（小写十六进制）；第一次请求为 None。
    pub rejected_fingerprint: Option<String>,
}

/// 来源给出的凭据：key 写进 `Authorization: Bearer`（Anthropic 协议）或适配器的鉴权位置；`headers` 原样加在请求上。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SuppliedAuth {
    pub api_key: Option<RedactedString>,
    pub headers: Vec<(String, RedactedString)>,
}

/// 账号模型每请求鉴权的来源。
#[async_trait]
pub trait HeaderSource: Send + Sync {
    /// 这次请求的凭据。拿不到时返回 [`ModelError::Authentication`]（文字给用户看）。
    async fn request_auth(&self, request: HeaderRequest) -> Result<SuppliedAuth, ModelError>;
}
