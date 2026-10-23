//! count_tokens（docs/specs/rust-cli/context-usage.md §3）：`/context` 的计数。只有
//! Anthropic 协议有这个接口；其他协议返回 `Unsupported`，调用方改用本地估算。不重试：失败时调用方估算。

use rust_cli_base::js::json::stringify;
use rust_cli_kernel_api::model::{CountTokensRequest, ModelError, ToolFlags};
use rust_cli_kernel_api::provider::{ModelEndpoint, WireProtocol};
use rust_cli_net::HttpRequest;
use rust_cli_net::header;
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use crate::anthropic::{API_VERSION, Anthropic, BETA_HEADER, HEADER_TIMEOUT, VERSION_HEADER};
use crate::endpoint::anthropic_count_url;
use crate::protocol::{Failure, Protocol};
use crate::transport::Route;

/// 计数接口的 beta（count_tokens 请求带它）。
const BETA: &str = "token-counting-2024-11-01";
/// 错误响应正文最多读这么多字节。
const BODY_LIMIT: usize = 64 * 1024;

/// 请求体 `{model, messages, tools}`；没有消息时发一条 `foo`。工具只有名字、描述与 schema。
pub(crate) fn encode(
    endpoint: &ModelEndpoint,
    request: CountTokensRequest,
) -> Result<HttpRequest, ModelError> {
    let messages = if request.messages.is_empty() {
        vec![json!({"role": "user", "content": "foo"})]
    } else {
        request
            .messages
            .into_iter()
            .map(crate::anthropic_request::message)
            .collect::<Result<Vec<_>, _>>()?
    };
    let tools = request
        .tools
        .into_iter()
        .map(|tool| crate::anthropic_request::tool(tool, ToolFlags::default()))
        .collect();
    let mut body = Map::new();
    body.insert("model".into(), Value::String(endpoint.model.clone()));
    body.insert("messages".into(), Value::Array(messages));
    body.insert("tools".into(), Value::Array(tools));
    let mut http = HttpRequest::post(
        anthropic_count_url(&endpoint.base_url),
        stringify(&Value::Object(body)),
    )
    .header(header::ACCEPT, header::JSON)
    .header(header::CONTENT_TYPE, header::JSON)
    .header(VERSION_HEADER, API_VERSION)
    .header(BETA_HEADER, BETA);
    http.timeout = Some(HEADER_TIMEOUT);
    Ok(http)
}

/// 取端点、发一次请求、读 `input_tokens`。
pub(crate) async fn count(
    route: Route,
    request: CountTokensRequest,
    cancel: CancellationToken,
) -> Result<u64, ModelError> {
    let endpoint = cancel
        .run_until_cancelled(route.endpoints.endpoint(&request.selection, None))
        .await
        .ok_or(ModelError::Cancelled)??;
    if endpoint.protocol != WireProtocol::AnthropicMessages {
        return Err(ModelError::Unsupported {
            what: "count_tokens".to_owned(),
        });
    }
    let mut request = request;
    crate::media::project(&mut request.messages, &endpoint);
    let encoded = encode(&endpoint, request)?;
    // 计数没有会话、回合与来源：每请求头只有 ID 与 `other`（model.md §2.4）。
    let http = crate::request_headers::outbound(
        &Anthropic,
        &encoded,
        &endpoint,
        route.headers.as_deref(),
        &crate::attribution::Origin::none(),
    );
    let response = cancel
        .run_until_cancelled(route.egress.send(http))
        .await
        .ok_or(ModelError::Cancelled)?
        .map_err(|error| Failure::network(&error).error)?;
    let status = response.status();
    let success = response.is_success();
    let headers = response.headers().clone();
    let body = cancel
        .run_until_cancelled(response.text(BODY_LIMIT))
        .await
        .ok_or(ModelError::Cancelled)?
        .map_err(|error| Failure::network(&error).error)?;
    if !success {
        let now = crate::transport::now(route.clock.as_ref());
        return Err(Anthropic.classify(status, &headers, &body, now).error);
    }
    serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|value| value.get("input_tokens")?.as_u64())
        .ok_or_else(|| ModelError::Protocol {
            message: "count_tokens: the response has no input_tokens".to_owned(),
        })
}
