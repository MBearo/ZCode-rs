// 语义复制自 apps/zcode-cli/packages/adapters/src/auth/cli-oauth.ts 与
// apps/zcode-cli/packages/bootstrap/src/auth-login-polling.ts（33a9541140）。
//! CLI OAuth 的服务端轮询（platform.md §5.6）：`/oauth/cli/init` 拿授权地址，`/oauth/cli/poll/{flow}` 轮询。
//!
//! 错误文案与 Node `CliOAuthError` 相同；轮询时 408、429、5xx 与网络错误按间隔重试，其他错误立刻失败。

use std::sync::Arc;

use rust_cli_base::proto::http;
use rust_cli_base::secret::RedactedString;
use rust_cli_net::{Egress, HeaderValue, HttpRequest, header};
use serde_json::{Map, Value, json};

use crate::token::http::{MAX_RESPONSE_BYTES, read_limited};

/// `CliOAuthError`：文案，以及 HTTP 状态（有的话）。没有状态的是网络错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OAuthError {
    pub message: String,
    pub status: Option<u16>,
    /// 网络错误（Node 里不是 `CliOAuthError`，轮询时重试）。
    pub network: bool,
}

impl OAuthError {
    fn new(message: impl Into<String>, status: Option<u16>) -> Self {
        Self {
            message: message.into(),
            status,
            network: false,
        }
    }

    /// 轮询时可以重试：网络错误、408、429、5xx。
    pub fn transient(&self) -> bool {
        self.network
            || matches!(
                self.status,
                Some(http::REQUEST_TIMEOUT | http::TOO_MANY_REQUESTS | http::SERVER_ERROR_MIN..)
            )
    }
}

pub(crate) struct Init {
    pub authorize_url: String,
    /// 秒。
    pub expires_at: f64,
    pub flow_id: String,
    pub poll_interval_sec: f64,
}

/// `ready` 时的结果。
pub(crate) struct Ready {
    pub token: RedactedString,
    /// `{avatar?, email?, name?, user_id}`（Node 的键序，`user_id` 去空白）：给输出用。
    pub user: Map<String, Value>,
    /// 后端的原始用户对象（App 按它存 user_info，工作线 F50）。
    pub raw_user: Map<String, Value>,
    pub access_token: RedactedString,
    pub refresh_token: Option<RedactedString>,
}

pub(crate) enum Poll {
    Pending,
    Failed,
    Ready(Box<Ready>),
}

pub(crate) struct OAuthClient {
    pub egress: Arc<Egress>,
    /// `<origin>/api/v1`，不带末尾的 `/`。
    pub base_url: String,
    /// `zai` / `bigmodel`。
    pub provider: &'static str,
}

impl OAuthClient {
    pub async fn init(&self, poll_token: &str) -> Result<Init, OAuthError> {
        let request = HttpRequest::post(
            format!("{}/oauth/cli/init", self.base_url),
            json!({"provider": self.provider}).to_string(),
        )
        .header(header::CONTENT_TYPE, header::JSON);
        let data = self.envelope(request, poll_token).await?;
        let invalid = || OAuthError::new("Invalid OAuth init response data", None);
        let flow_id = text(&data, "flow_id").ok_or_else(invalid)?;
        let authorize_url = text(&data, "authorize_url").ok_or_else(invalid)?;
        let number = |name: &str| data[name].as_f64().filter(|value| value.is_finite());
        let (Some(expires_at), Some(poll_interval_sec)) =
            (number("expires_at"), number("poll_interval_sec"))
        else {
            return Err(invalid());
        };
        if poll_interval_sec < 1.0 {
            return Err(invalid());
        }
        let https = url::Url::parse(&authorize_url).is_ok_and(|url| url.scheme() == "https");
        if !https {
            return Err(OAuthError::new("Invalid OAuth authorization URL", None));
        }
        Ok(Init {
            authorize_url,
            expires_at,
            flow_id,
            poll_interval_sec,
        })
    }

    pub async fn poll(&self, flow_id: &str, poll_token: &str) -> Result<Poll, OAuthError> {
        let url = format!(
            "{}/oauth/cli/poll/{}",
            self.base_url,
            crate::account::encode_uri_component(flow_id)
        );
        let data = self.envelope(HttpRequest::get(url), poll_token).await?;
        match data["status"].as_str() {
            Some("pending") => Ok(Poll::Pending),
            Some("failed") => Ok(Poll::Failed),
            Some("ready") => self.ready(&data).map(|ready| Poll::Ready(Box::new(ready))),
            _ => Err(OAuthError::new("Invalid OAuth poll response data", None)),
        }
    }

    fn ready(&self, data: &Value) -> Result<Ready, OAuthError> {
        let invalid = || OAuthError::new("Invalid OAuth ready response data", None);
        let user = data["user"].as_object().ok_or_else(invalid)?;
        let provider = &data[self.provider];
        let access_token = text(provider, "access_token")
            .or_else(|| text(provider, "accessToken"))
            .ok_or_else(invalid)?;
        let refresh_token =
            text(provider, "refresh_token").or_else(|| text(provider, "refreshToken"));
        let token = text(data, "token").ok_or_else(invalid)?;
        let user_id = text(&data["user"], "user_id").ok_or_else(invalid)?;
        let mut profile = Map::new();
        for name in ["avatar", "email", "name"] {
            if let Some(value) = user.get(name).filter(|value| value.is_string()) {
                profile.insert(name.to_owned(), value.clone());
            }
        }
        profile.insert("user_id".to_owned(), Value::String(user_id));
        Ok(Ready {
            token: RedactedString::new(token),
            user: profile,
            raw_user: user.clone(),
            access_token: RedactedString::new(access_token),
            refresh_token: refresh_token.map(RedactedString::new),
        })
    }

    /// 发请求，读 `{code, data, msg}` 信封：HTTP 非 2xx、不是 JSON、`code` 不是数字或不是 0 都是错误。每个请求最多等
    /// 180 s 的响应头（net `timeout::OAUTH`，照 Node 的 HTTP 客户端缺省，F58）。
    async fn envelope(&self, request: HttpRequest, poll_token: &str) -> Result<Value, OAuthError> {
        let mut request = request.header(
            header::AUTHORIZATION,
            HeaderValue::Secret(header::bearer(poll_token)),
        );
        request.timeout = Some(rust_cli_net::timeout::OAUTH);
        let network = |error: rust_cli_net::NetError| OAuthError {
            message: error.to_string(),
            status: None,
            network: true,
        };
        let response = self.egress.send(request).await.map_err(network)?;
        let status = response.status();
        if !response.is_success() {
            return Err(OAuthError::new(
                format!("OAuth HTTP error {status}"),
                Some(status),
            ));
        }
        // 超过上限：拒收，按网络错误处理（Node 的 `too_large` 不是 `CliOAuthError`，轮询时按间隔重试；F58）。
        let Some(body) = read_limited(response).await.map_err(network)? else {
            return Err(OAuthError {
                message: format!("HTTP response is too large: bytes>{MAX_RESPONSE_BYTES}"),
                status: None,
                network: true,
            });
        };
        let envelope: Value = serde_json::from_str(&body)
            .map_err(|_| OAuthError::new("OAuth response is not valid JSON", None))?;
        let Some(code) = envelope.as_object().and_then(|_| envelope["code"].as_f64()) else {
            return Err(OAuthError::new(
                "Invalid OAuth response envelope",
                Some(status),
            ));
        };
        if code != 0.0 {
            let message = envelope["msg"].as_str().map_or_else(
                || format!("OAuth business error {}", envelope["code"]),
                str::to_owned,
            );
            return Err(OAuthError::new(message, Some(status)));
        }
        Ok(envelope["data"].clone())
    }
}

/// 去掉首尾空白后非空的字符串字段。
fn text(value: &Value, name: &str) -> Option<String> {
    value[name]
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}
