//! 请求与响应的值类型。上层只经这些类型收发 HTTP，不依赖 reqwest。

use std::time::Duration;

use bytes::Bytes;
use reqwest::header::{HeaderMap, HeaderName};
use rust_cli_base::secret::RedactedString;

use crate::NetError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

impl Method {
    pub(crate) fn to_reqwest(self) -> reqwest::Method {
        match self {
            Self::Get => reqwest::Method::GET,
            Self::Post => reqwest::Method::POST,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }
}

/// header 值。秘密值（API key、Bearer token）用 [`HeaderValue::Secret`]：发送时标记为敏感，
/// 任何 Debug 输出都不含原值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeaderValue {
    Plain(String),
    Secret(RedactedString),
}

impl From<&str> for HeaderValue {
    fn from(value: &str) -> Self {
        Self::Plain(value.to_owned())
    }
}

impl From<String> for HeaderValue {
    fn from(value: String) -> Self {
        Self::Plain(value)
    }
}

/// 一个出站请求。`body` 是 [`Bytes`]，重试时克隆不复制内容。
#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub method: Method,
    pub url: String,
    /// 本请求的 header；与出口策略的 header 同名时以这里为准。
    pub headers: Vec<(String, HeaderValue)>,
    pub body: Bytes,
    /// 从发出请求到收到响应头的时限；为空时不限（调用方用取消令牌结束）。
    pub timeout: Option<Duration>,
}

impl HttpRequest {
    pub fn post(url: impl Into<String>, body: impl Into<Bytes>) -> Self {
        Self {
            method: Method::Post,
            url: url.into(),
            headers: Vec::new(),
            body: body.into(),
            timeout: None,
        }
    }

    pub fn get(url: impl Into<String>) -> Self {
        Self {
            method: Method::Get,
            url: url.into(),
            headers: Vec::new(),
            body: Bytes::new(),
            timeout: None,
        }
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<HeaderValue>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

/// 把（名，值）转成 reqwest 的 header；名或值非法时报 `invalid`。
pub(crate) fn header_pair(
    name: &str,
    value: &HeaderValue,
) -> Result<(HeaderName, reqwest::header::HeaderValue), String> {
    let header = HeaderName::from_bytes(name.as_bytes()).map_err(|_| format!("header {name}"))?;
    let value = match value {
        HeaderValue::Plain(text) => reqwest::header::HeaderValue::from_str(text),
        HeaderValue::Secret(secret) => {
            reqwest::header::HeaderValue::from_str(secret.expose()).map(|mut value| {
                value.set_sensitive(true);
                value
            })
        }
    }
    .map_err(|_| format!("value of header {name}"))?;
    Ok((header, value))
}

/// 响应头（名字大小写不敏感）。
#[derive(Debug, Clone, Default)]
pub struct ResponseHeaders(HeaderMap);

impl ResponseHeaders {
    /// 测试与适配器内构造用；名或值非法的项被忽略。
    pub fn from_pairs<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            if let Ok((name, value)) = header_pair(name, &HeaderValue::from(value)) {
                map.append(name, value);
            }
        }
        Self(map)
    }

    /// 第一个同名 header 的值；不是可见 ASCII 时视为没有。
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).and_then(|value| value.to_str().ok())
    }

    /// 全部 header（名字小写，值按 UTF-8 宽松解码），同名的相邻：模型轨迹记回复头用
    /// （docs/specs/rust-cli/model-io.md §4.2）。
    pub fn pairs(&self) -> Vec<(String, String)> {
        self.0
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    String::from_utf8_lossy(value.as_bytes()).into_owned(),
                )
            })
            .collect()
    }
}

/// 收到响应头之后的响应；响应体按块读取。丢弃它即关闭连接。
#[derive(Debug)]
pub struct HttpResponse {
    status: u16,
    headers: ResponseHeaders,
    inner: reqwest::Response,
}

impl HttpResponse {
    pub(crate) fn new(mut inner: reqwest::Response) -> Self {
        let headers = ResponseHeaders(std::mem::take(inner.headers_mut()));
        Self {
            status: inner.status().as_u16(),
            headers,
            inner,
        }
    }

    pub fn status(&self) -> u16 {
        self.status
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn headers(&self) -> &ResponseHeaders {
        &self.headers
    }

    /// 下一块响应体；读完时返回 `None`。块的边界由传输层决定，与内容无关。
    pub async fn chunk(&mut self) -> Result<Option<Bytes>, NetError> {
        self.inner
            .chunk()
            .await
            .map_err(|error| NetError::from_reqwest(&error))
    }

    /// 读取至多 `limit` 字节的响应体，按 UTF-8 宽松解码（错误响应的正文用于分类与提示）。
    pub async fn text(mut self, limit: usize) -> Result<String, NetError> {
        let mut body = Vec::new();
        while body.len() < limit {
            let Some(chunk) = self.chunk().await? else {
                break;
            };
            let room = limit - body.len();
            body.extend_from_slice(&chunk[..chunk.len().min(room)]);
        }
        Ok(String::from_utf8_lossy(&body).into_owned())
    }
}
