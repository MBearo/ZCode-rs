//! 线协议的扩展点（docs/specs/rust-cli/model.md §1）。
//!
//! 一种协议负责三件事：把 canonical 请求编码成 HTTP 请求，写凭据 header，把 SSE 事件解码成
//! [`ModelEvent`]。失败的分类在 `errors`（按厂商与协议，修复线 F113），连接、重试、取消与读流由 `transport` 统一处理。
//! 三种协议：Anthropic Messages、OpenAI Chat Completions、OpenAI Responses。

use std::collections::VecDeque;

use rust_cli_kernel_api::model::{ModelError, ModelEvent, ModelRequest};
use rust_cli_kernel_api::provider::{Credential, ModelEndpoint, WireProtocol};
use rust_cli_net::sse::SseEvent;
use rust_cli_net::{HeaderValue, HttpRequest};

use crate::anthropic::Anthropic;
use crate::errors::{Vendor, Verdict};
use crate::openai_chat::OpenAiChat;
use crate::openai_responses::OpenAiResponses;
use crate::profiles::Profile;

pub(crate) trait Protocol: Send + Sync {
    /// 编码请求：URL、请求体与和凭据无关的 header。消费请求：大段文本移入请求体，不复制历史。
    /// `profile`：这次请求的方案（model.md §3.2，transport 按端点与入口的声明查表）。
    fn encode(
        &self,
        endpoint: &ModelEndpoint,
        request: ModelRequest,
        profile: &Profile,
    ) -> Result<HttpRequest, ModelError>;

    /// 凭据对应的 header（每次发送前按当前端点写入，凭据续期后换新）。
    fn credential_headers(&self, credential: &Credential) -> Vec<(String, HeaderValue)>;

    /// 这次响应流的解码器。`vendor`：错误块与结束原因先经这个厂商的分类器（provider-errors.md §2.2，修复线 F113）。
    fn decoder(&self, vendor: Vendor) -> Box<dyn StreamDecoder>;
}

pub(crate) trait StreamDecoder: Send {
    /// 解码一个 SSE 事件，产生的模型事件追加到 `out`。错误事件与协议违例返回分类结果（修复线 F113：以前是
    /// `ModelError`，transport 要按中性分类决定流内重试，provider-errors.md §3.2）。
    fn decode(&mut self, event: SseEvent, out: &mut VecDeque<ModelEvent>) -> Result<(), Verdict>;

    /// 已收到流的终止事件，之后的字节不再读取。
    fn finished(&self) -> bool;
}

/// 线协议对应的实现。
pub(crate) fn for_wire(protocol: WireProtocol) -> &'static dyn Protocol {
    match protocol {
        WireProtocol::AnthropicMessages => &Anthropic,
        WireProtocol::OpenAiChatCompletions => &OpenAiChat,
        WireProtocol::OpenAiResponses => &OpenAiResponses,
    }
}

/// `Authorization: Bearer <key>`（OpenAI 两种协议）。
pub(crate) fn bearer(credential: &Credential) -> Vec<(String, HeaderValue)> {
    match credential {
        Credential::None => Vec::new(),
        Credential::ApiKey(key) => vec![(
            rust_cli_net::header::AUTHORIZATION.to_owned(),
            HeaderValue::Secret(rust_cli_net::header::bearer(key.expose())),
        )],
    }
}
