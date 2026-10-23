//! 线协议的扩展点（docs/specs/rust-cli/model.md §1）。
//!
//! 一种协议负责四件事：把 canonical 请求编码成 HTTP 请求，写凭据 header，把 SSE 事件解码成
//! [`ModelEvent`]，把失败的 HTTP 响应分类。连接、重试、取消与读流由 `transport` 统一处理，与协议无关。
//! 三种协议：Anthropic Messages、OpenAI Chat Completions、OpenAI Responses。

use std::collections::VecDeque;
use std::time::{Duration, SystemTime};

use rust_cli_kernel_api::model::{ModelError, ModelEvent, ModelRequest};
use rust_cli_kernel_api::provider::{Credential, ModelEndpoint, WireProtocol};
use rust_cli_net::sse::SseEvent;
use rust_cli_net::{HeaderValue, HttpRequest, NetError, ResponseHeaders};

use crate::anthropic::Anthropic;
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

    /// 这次响应流的解码器。
    fn decoder(&self) -> Box<dyn StreamDecoder>;

    /// 非 2xx 响应的分类。`body` 是截断后的响应正文；`now` 取自注入的 Clock（Retry-After 的 HTTP 日期按它换算，
    /// F7：以前直接读系统时间）。
    fn classify(
        &self,
        status: u16,
        headers: &ResponseHeaders,
        body: &str,
        now: SystemTime,
    ) -> Failure;
}

pub(crate) trait StreamDecoder: Send {
    /// 解码一个 SSE 事件，产生的模型事件追加到 `out`。错误事件与协议违例返回错误。
    fn decode(&mut self, event: SseEvent, out: &mut VecDeque<ModelEvent>)
    -> Result<(), ModelError>;

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

/// 错误正文里给出的消息最多保留这么多字符（Anthropic 与 OpenAI 两种错误相同，F55：以前各写一份）。
pub(crate) const ERROR_MESSAGE_LIMIT: usize = 2000;

/// 一次失败的请求：给调用方的错误，以及能否重试。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Failure {
    pub error: ModelError,
    pub retry: Retry,
    /// 服务端要求的等待（Retry-After）。
    pub retry_after: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Retry {
    Never,
    /// 临时性失败：网络、限流、5xx 等。
    Transient,
    /// 服务过载（Anthropic 529 / `overloaded_error`）：只有用户在等结果的请求才重试。
    Overloaded,
}

impl Failure {
    /// 传输层错误：建连、超时、读写中断可以重试；TLS 与无效请求不重试。
    pub fn network(error: &NetError) -> Self {
        let (error_value, retry) = match error {
            NetError::Request(message) | NetError::Policy(message) => (
                ModelError::InvalidRequest {
                    message: message.clone(),
                },
                Retry::Never,
            ),
            NetError::Tls(_) => (
                ModelError::Network {
                    message: error.to_string(),
                },
                Retry::Never,
            ),
            NetError::Connect(_) | NetError::Timeout | NetError::Io(_) => (
                ModelError::Network {
                    message: error.to_string(),
                },
                Retry::Transient,
            ),
        };
        Self {
            error: error_value,
            retry,
            retry_after: None,
        }
    }
}
