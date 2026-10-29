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
mod anthropic_betas;
mod anthropic_output;
mod anthropic_request;
mod anthropic_shape;
mod anthropic_stream;
mod anthropic_write;
mod attribution;
mod endpoint;
mod errors;
mod idle;
mod media;
mod metadata;
mod model_io;
mod model_io_reply;
mod model_io_tape;
mod openai_blocks;
mod openai_chat;
mod openai_chat_stream;
mod openai_lower;
mod openai_responses;
mod openai_responses_stream;
mod patch;
mod profiles;
mod protocol;
mod request_headers;
mod system_fold;
mod transport;
mod transport_read;

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
pub use idle::{
    BYTE_IDLE_MAX, BYTE_IDLE_MIN, ByteIdle, EVENT_IDLE_FLOOR, STALL_NOTICE, StreamIdle,
};
pub use metadata::DeviceIdSource;
pub use model_io::{ModelIoRecord, ModelIoSink};
pub use profiles::ANTHROPIC_OFFICIAL_BASE_URL;
pub use request_headers::{RequestHeaders, RequestIdSource, SourceFacts, source_headers};
pub use rust_cli_net::retry::RetryPolicy;

use crate::transport::Route;

/// 模型端口。每次请求按 `selection` 向端点来源取一次端点与凭据。
pub struct ProviderModel {
    route: Result<Route, String>,
}

impl ProviderModel {
    /// `clock` 是入口注入的时钟：Retry-After 的 HTTP 日期按它换算（F7，审查 R2-11）。
    pub fn new(
        egress: Arc<Egress>,
        endpoints: Arc<dyn ModelEndpoints>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            route: Ok(Route {
                egress,
                endpoints,
                retry: RetryPolicy::default(),
                idle: StreamIdle::default(),
                clock,
                assume_first_party: false,
                assume_base_url: None,
                sandbox: false,
                max_output_tokens: None,
                device: None,
                headers: None,
                model_io: None,
            }),
        }
    }

    /// 固定的 Anthropic 端点（显式覆盖与测试）：请求体的 `model` 取选择里的模型。
    pub fn anthropic(
        egress: Arc<Egress>,
        endpoint: AnthropicEndpoint,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self::new(egress, Arc::new(FixedEndpoint::new(endpoint)), clock)
    }

    /// 没有可用的端点（例如没有 API key）：每次请求都返回 [`ModelError::Authentication`]。
    pub fn unconfigured(reason: impl Into<String>) -> Self {
        Self {
            route: Err(reason.into()),
        }
    }

    /// 模型请求的来源头与每请求头的 ID（工作线 F64，model.md §2.4）：入口给来源头（[`source_headers`]，含
    /// `user-agent: ZCode/<版本>`）、每个进程一个的 trace ID 与 `x-request-id` 的来源。没有时这两层都不发（测试）。
    #[must_use]
    pub fn with_request_headers(mut self, headers: RequestHeaders) -> Self {
        if let Ok(route) = &mut self.route {
            route.headers = Some(Arc::new(headers));
        }
        self
    }

    /// 替换重试策略（默认见 [`RetryPolicy::default`]）。
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        if let Ok(route) = &mut self.route {
            route.retry = retry;
        }
        self
    }

    /// 端点一律按 Anthropic 官方端点处理（`cache_control.scope`、`diagnostics`、流空闲上限，model.md §3.2）。
    /// 只给测试用：入口在 `_ZCODE_ASSUME_FIRST_PARTY_BASE_URL` 为真时对显式覆盖的端点打开（model.md §6），让本地
    /// 替身端点也收到第一方形状。
    pub fn with_assumed_first_party(mut self) -> Self {
        if let Ok(route) = &mut self.route {
            route.assume_first_party = true;
        }
        self
    }

    /// 查请求方案与厂商（错误码表）时把端点的 base URL 当成 `base_url`，请求仍发到真实地址。只给测试用：入口在
    /// `_ZCODE_ASSUME_BASE_URL` 有值时打开（model.md §6，docs/specs/rust-cli/provider-errors.md D13），让本地夹具走到
    /// 智谱的方案与码表。[`with_assumed_first_party`](Self::with_assumed_first_party) 优先。
    #[must_use]
    pub fn with_assumed_base_url(mut self, base_url: impl Into<String>) -> Self {
        if let Ok(route) = &mut self.route {
            route.assume_base_url = Some(base_url.into());
        }
        self
    }

    /// 替换流空闲看门狗的配置（默认见 [`StreamIdle::default`]；入口按环境变量给出，
    /// docs/specs/rust-cli/stall-visibility.md §2.2）。
    /// 入口读到 `IS_SANDBOX`（非空）时：opus 系列连续过载后照常重试，不报 `Repeated 529 Overloaded errors`
    /// （docs/specs/rust-cli/cli-args.md §9）。
    #[must_use]
    pub fn with_overload_retries_in_sandbox(mut self, sandbox: bool) -> Self {
        if let Ok(route) = &mut self.route {
            route.sandbox = sandbox;
        }
        self
    }

    /// 入口读到 `ZCODE_MAX_OUTPUT_TOKENS`（cli-args.md §9.1，修复线 F42）：每个请求的 `max_tokens` 压到不超过它。每个
    /// 请求都是 `min(请求自己的值 || 有效上限, 有效上限)`，旁路请求（WebSearch、prompt hook、标题）因此也被压低；
    /// 主循环自己的值由 query 按卡片算（可能抬高到缺省以上）。平台层的补丁另按配置取小（model.md §9）。
    #[must_use]
    pub fn with_max_output_tokens(mut self, limit: Option<u32>) -> Self {
        if let Ok(route) = &mut self.route {
            route.max_output_tokens = limit;
        }
        self
    }

    /// 设备身份的来源（工作线 F52，model.md §3.3）：有它时每个 messages 请求都带 `metadata.user_id`（只有 Anthropic
    /// 协议，F52b），值是设备身份加请求的 `options.session_id`；调用方自己给了 `options.user_id` 时用调用方的。入口总是
    /// 给（platform 的 `DeviceMid`）；没有时不写 metadata（测试）。
    #[must_use]
    pub fn with_device_id(mut self, source: DeviceIdSource) -> Self {
        if let Ok(route) = &mut self.route {
            route.device = Some(source);
        }
        self
    }

    /// 模型轨迹的记录方（工作线 F91，docs/specs/rust-cli/model-io.md §6）：每次 HTTP 发送交一条记录。入口按模式给；
    /// 没有时不记（测试、测试环境）。
    #[must_use]
    pub fn with_model_io(mut self, sink: Arc<dyn ModelIoSink>) -> Self {
        if let Ok(route) = &mut self.route {
            route.model_io = Some(sink);
        }
        self
    }

    pub fn with_stream_idle(mut self, idle: StreamIdle) -> Self {
        if let Ok(route) = &mut self.route {
            route.idle = idle;
        }
        self
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
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ModelStreamItem, ModelError>> {
        match &self.route {
            Ok(route) => {
                let mut request = request;
                if let Some(limit) = route.max_output_tokens {
                    request.max_tokens = request.max_tokens.min(limit);
                }
                transport::stream(route, request, cancel)
            }
            Err(reason) => stream::iter([Err(ModelError::Authentication {
                message: reason.clone(),
            })])
            .boxed(),
        }
    }

    /// 按选择查能力（docs/specs/rust-cli/model.md §11）：交给端点来源，不取凭据。
    fn capabilities(
        &self,
        selection: &ModelSelection,
    ) -> BoxFuture<'static, Option<ModelCapabilities>> {
        match &self.route {
            Ok(route) => {
                let endpoints = route.endpoints.clone();
                let selection = selection.clone();
                async move { endpoints.capabilities(&selection).await }.boxed()
            }
            Err(_) => async { None }.boxed(),
        }
    }
}
