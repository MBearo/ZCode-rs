//! 模型端口的实现（docs/specs/rust-cli/README.md §3，细则见 docs/specs/rust-cli/model.md）。
//!
//! canonical 请求是 Anthropic Messages。每种线协议实现 `protocol::Protocol`：编码请求、写凭据 header、
//! 解码 SSE、分类错误；取端点、连接、重试、取消与读流由 `transport` 统一处理。HTTP、SSE 与退避都经
//! `rust-cli-net`。
//!
//! 每次请求先经 [`ModelEndpoints`] 取端点（平台层按 Provider registry 与账号解析，或显式覆盖的
//! [`FixedEndpoint`]），再按端点的协议分派：anthropic-messages、openai-chat-completions、openai-responses。
//! OpenAI 协议没有的能力在编码器里降级（model.md §10）。

mod anthropic;
mod attribution;
mod endpoint;
mod errors;
mod media;
mod metadata;
mod openai;
mod patch;
mod profiles;
mod protocol;
mod request_headers;
mod trace;
mod transport;
mod wire;

use std::sync::Arc;

use futures_util::future::{BoxFuture, FutureExt};
use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::model::{
    ModelError, ModelEvent, ModelPort, ModelRequest, ModelSelection, ModelStreamItem,
};
use rust_cli_kernel_api::provider::{ModelCapabilities, ModelEndpoints};
use rust_cli_net::Egress;
use tokio_util::sync::CancellationToken;

pub use endpoint::{AnthropicEndpoint, CapabilityRules, FixedEndpoint, anthropic_v1_base_url};
pub use metadata::DeviceIdSource;
pub use profiles::ANTHROPIC_OFFICIAL_BASE_URL;
pub use request_headers::{RequestHeaders, RequestIdSource, SourceFacts, source_headers};
pub use rust_cli_net::retry::RetryPolicy;
pub use trace::{ModelIoRecord, ModelIoSink};
pub use transport::idle::{
    BYTE_IDLE_MAX, BYTE_IDLE_MIN, ByteIdle, EVENT_IDLE_FLOOR, STALL_NOTICE, StreamIdle,
};

use crate::transport::Route;
pub use crate::transport::RouteOptions;

/// 模型端口。每次请求按 `selection` 向端点来源取一次端点与凭据。
pub struct ProviderModel {
    route: Route,
}

impl ProviderModel {
    /// `clock` 是入口注入的时钟：Retry-After 的 HTTP 日期按它换算（F7，审查 R2-11）；`options` 是入口级的选项
    /// （测试用 `RouteOptions::default()`）。
    pub fn new(
        egress: Arc<Egress>,
        endpoints: Arc<dyn ModelEndpoints>,
        clock: Arc<dyn Clock>,
        options: RouteOptions,
    ) -> Self {
        Self {
            route: Route {
                egress,
                endpoints,
                clock,
                options,
            },
        }
    }
}

/// 没有可用的端点（例如没有 API key、选不出模型）：每次请求都返回 [`ModelError::Authentication`]，说明原因；能力答不出。
/// F129（审查 E10）：以前是 `ProviderModel` 里的 `Result<Route, String>`，每个选项都要先看有没有端点。
pub struct UnconfiguredModel {
    reason: String,
}

impl UnconfiguredModel {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl ModelPort for UnconfiguredModel {
    fn stream(
        &self,
        _request: ModelRequest,
        _cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
        stream::iter([Err(ModelError::Authentication {
            message: self.reason.clone(),
        })])
        .boxed()
    }
}

impl ModelPort for ProviderModel {
    fn stream(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
        self.stream_with_notices(request, cancel)
            .filter_map(|item| async move {
                match item {
                    Ok(ModelStreamItem::Event(event)) => Some(Ok(event)),
                    Ok(ModelStreamItem::Notice(_)) => None,
                    Err(error) => Some(Err(error)),
                }
            })
            .boxed()
    }

    fn stream_with_notices(
        &self,
        mut request: ModelRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ModelStreamItem, ModelError>> {
        if let Some(limit) = self.route.options.max_output_tokens {
            request.max_tokens = request.max_tokens.min(limit);
        }
        transport::stream(&self.route, request, cancel)
    }

    /// 按选择查能力（docs/specs/rust-cli/model.md §11）：交给端点来源，不取凭据。
    fn capabilities(
        &self,
        selection: &ModelSelection,
    ) -> BoxFuture<'static, Option<ModelCapabilities>> {
        let endpoints = self.route.endpoints.clone();
        let selection = selection.clone();
        async move { endpoints.capabilities(&selection).await }.boxed()
    }
}
