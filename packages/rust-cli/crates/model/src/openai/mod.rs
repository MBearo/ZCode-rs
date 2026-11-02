//! 两种 OpenAI 协议（docs/specs/rust-cli/model.md §10）：Chat Completions 与 Responses。请求体与解码各写各的；
//! 发送（URL 之外的 header 与时限）、凭据的写法与“没有服务端工具”的检查共用这里的一份（F129，审查 E12）。

use rust_cli_kernel_api::model::{ModelError, ModelRequest};
use rust_cli_kernel_api::provider::{Credential, ModelEndpoint};
use rust_cli_net::{HeaderValue, HttpRequest, header};
use serde_json::Value;

use crate::wire::HEADER_TIMEOUT;

mod blocks;
pub(crate) mod chat;
mod chat_stream;
pub(crate) mod lower;
pub(crate) mod responses;
mod responses_stream;

/// OpenAI 的文件输入要文件名；document 块没有，取 AI SDK 的缺省值（model.md §10.6，两种协议相同）。
pub(crate) const PDF_NAME: &str = "document.pdf";

/// 一种协议的请求体：模型 ID、请求、Provider 的补丁 → JSON 文本。
type Body = fn(&str, ModelRequest, &[Value]) -> Result<String, ModelError>;

/// 编码一次请求：先查服务端工具，再按协议写请求体，URL 由协议给出。
pub(crate) fn encode(
    endpoint: &ModelEndpoint,
    request: ModelRequest,
    url: fn(&str) -> String,
    body: Body,
) -> Result<HttpRequest, ModelError> {
    // OpenAI 协议没有 Anthropic 的服务端工具（WebSearch，model.md §10）。
    if !request.options.server_tools.is_empty() {
        return Err(ModelError::Unsupported {
            what: "server tools (web_search) on the OpenAI protocols".to_owned(),
        });
    }
    let body = body(&endpoint.model, request, &endpoint.body_patches)?;
    let mut http = HttpRequest::post(url(&endpoint.base_url), body)
        .header(header::ACCEPT, header::EVENT_STREAM)
        .header(header::CONTENT_TYPE, header::JSON);
    http.timeout = Some(HEADER_TIMEOUT);
    Ok(http)
}

/// `Authorization: Bearer <key>`。
pub(crate) fn bearer(credential: &Credential) -> Vec<(String, HeaderValue)> {
    match credential {
        Credential::None => Vec::new(),
        Credential::ApiKey(key) => vec![(
            header::AUTHORIZATION.to_owned(),
            HeaderValue::Secret(header::bearer(key.expose())),
        )],
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
