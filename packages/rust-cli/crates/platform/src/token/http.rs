// 语义复制自 apps/zcode-cli/packages/adapters/src/auth/coding-plan-api-key.ts（33a9541140）的 `request`。
//! 项目令牌的 HTTP（platform.md §5.4）：经 net 发出，按 Node 的规则把响应分成 JSON、404 与故障。
//!
//! 每个请求 15 秒（net `timeout::ACCOUNT`）、响应体最多 64 KiB。网络错误、超时、429、5xx 是临时故障（调用方可能沿用未过期的
//! 缓存）；401 单独给出（Z.ai 业务 token 的作废要用）；其他非 2xx 与非 JSON 的响应体是失败。错误里不带
//! 响应体、URL 或令牌。

use std::sync::Arc;

use rust_cli_base::proto::http;
use rust_cli_base::secret::RedactedString;
use rust_cli_base::units;
use rust_cli_net::{Egress, HeaderValue, HttpRequest, HttpResponse, NetError, header};
use serde_json::Value;

use crate::consts;

/// 临时故障的 5xx 的最后一个码：600 起不是标准状态码，按失败处理。
const SERVER_ERROR_LAST: u16 = 599;

/// 账号接口响应体的上限：项目令牌、团队与 OAuth 共用（F55：以前 oauth 另写了一份）。
pub(crate) const MAX_RESPONSE_BYTES: usize = 64 * units::KIB;

/// 读响应体，最多 [`MAX_RESPONSE_BYTES`]；超过时为 None（拒收，不截断）。项目令牌、团队与 OAuth 共用，照 Node
/// `readResponseBody`（超过上限报 `too_large`）。修复线 F58：以前 OAuth 截断到上限再解析，报“不是 JSON”。
pub(crate) async fn read_limited(response: HttpResponse) -> Result<Option<String>, NetError> {
    let body = response.text(MAX_RESPONSE_BYTES + 1).await?;
    Ok((body.len() <= MAX_RESPONSE_BYTES).then_some(body))
}
/// customerInfo 的地址（相对 origin）：项目位置与团队列表都先查它（F55：以前写了两次）。
pub(crate) const CUSTOMER_INFO_PATH: &str = "/api/biz/customer/getCustomerInfo";
/// 团队请求带的组织与项目头（App 的 team 请求；F55：以前在两个文件里各写一次）。
pub(crate) const ORGANIZATION_HEADER: &str = "bigmodel-organization";
pub(crate) const PROJECT_HEADER: &str = "bigmodel-project";

/// 鉴权头的形式：customerInfo 要裸 token，其余多数用 Bearer（platform.md §5.4）。
#[derive(Clone, Copy)]
pub(crate) enum Auth<'a> {
    None,
    Raw(&'a str),
    Bearer(&'a str),
}

#[derive(Debug)]
pub(crate) enum Reply {
    Json(Value),
    /// 404：签发时表示 API key 已不存在（调用方删掉项目位置）。
    NotFound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fault {
    /// 网络错误、超时、429、5xx。
    Transient,
    /// 401。
    Unauthorized,
    /// 403（团队权益把 401 / 403 都当鉴权失败，platform.md §9.3；换取里与 `Failed` 相同）。
    Forbidden,
    /// 其他非 2xx、响应体过大或不是 JSON。
    Failed,
}

#[derive(Clone)]
pub(crate) struct Http {
    egress: Arc<Egress>,
}

impl Http {
    pub fn new(egress: Arc<Egress>) -> Self {
        Self { egress }
    }

    pub async fn get(&self, url: String, auth: Auth<'_>) -> Result<Reply, Fault> {
        self.call(HttpRequest::get(url), auth, &[]).await
    }

    pub async fn post(&self, url: String, auth: Auth<'_>, body: &Value) -> Result<Reply, Fault> {
        self.call(HttpRequest::post(url, body.to_string()), auth, &[])
            .await
    }

    /// 带额外 header 的 GET（团队接口的 `bigmodel-organization` / `bigmodel-project`）。
    pub async fn get_with(
        &self,
        url: String,
        auth: Auth<'_>,
        extra: &[(&str, &str)],
    ) -> Result<Reply, Fault> {
        self.call(HttpRequest::get(url), auth, extra).await
    }

    /// 带额外 header 的 POST。
    pub async fn post_with(
        &self,
        url: String,
        auth: Auth<'_>,
        extra: &[(&str, &str)],
        body: &Value,
    ) -> Result<Reply, Fault> {
        self.call(HttpRequest::post(url, body.to_string()), auth, extra)
            .await
    }

    async fn call(
        &self,
        request: HttpRequest,
        auth: Auth<'_>,
        extra: &[(&str, &str)],
    ) -> Result<Reply, Fault> {
        let mut request = request.header(header::CONTENT_TYPE, header::JSON);
        for (name, value) in extra {
            request = request.header(*name, *value);
        }
        request.timeout = Some(rust_cli_net::timeout::ACCOUNT);
        let authorization = match auth {
            Auth::None => None,
            Auth::Raw(token) => Some(RedactedString::new(token.to_owned())),
            Auth::Bearer(token) => Some(header::bearer(token)),
        };
        if let Some(value) = authorization {
            request = request.header(header::AUTHORIZATION, HeaderValue::Secret(value));
        }
        let response = self.egress.send(request).await.map_err(|error| {
            if error.is_transient() {
                Fault::Transient
            } else {
                Fault::Failed
            }
        })?;
        match response.status() {
            http::NOT_FOUND => return Ok(Reply::NotFound),
            http::UNAUTHORIZED => return Err(Fault::Unauthorized),
            http::FORBIDDEN => return Err(Fault::Forbidden),
            http::TOO_MANY_REQUESTS | http::SERVER_ERROR_MIN..=SERVER_ERROR_LAST => {
                return Err(Fault::Transient);
            }
            status if !http::is_success(status) => return Err(Fault::Failed),
            _ => {}
        }
        let body = read_limited(response)
            .await
            .map_err(|error| {
                if error.is_transient() {
                    Fault::Transient
                } else {
                    Fault::Failed
                }
            })?
            .ok_or(Fault::Failed)?;
        serde_json::from_str(&body)
            .map(Reply::Json)
            .map_err(|_| Fault::Failed)
    }
}

/// BigModel / Z.ai 管理接口与签发共用的成功码：200、0（数字或字符串）。
pub(crate) fn success_code(code: &Value) -> bool {
    match code {
        Value::Number(number) => {
            matches!(number.as_f64(), Some(value) if consts::BUSINESS_SUCCESS_CODES.contains(&value))
        }
        Value::String(text) => text == "0" || text == "200",
        _ => false,
    }
}

/// 管理接口的信封 `{code, data}`：成功码时给出 `data`（可以没有），否则为空（`project_token_invalid_response`）。
pub(crate) fn envelope_data(reply: &Reply) -> Option<&Value> {
    match reply {
        Reply::Json(value) if value.is_object() && success_code(&value["code"]) => {
            Some(&value["data"])
        }
        Reply::Json(_) | Reply::NotFound => None,
    }
}
