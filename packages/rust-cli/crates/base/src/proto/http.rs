//! HTTP 的状态码、原因短语与通用请求头名。

use std::ops::Range;

/// 切换协议（WebSocket 握手成功）。
pub const SWITCHING_PROTOCOLS: u16 = 101;
pub const OK: u16 = 200;
pub const ACCEPTED: u16 = 202;
pub const MOVED_PERMANENTLY: u16 = 301;
pub const FOUND: u16 = 302;
pub const SEE_OTHER: u16 = 303;
pub const TEMPORARY_REDIRECT: u16 = 307;
pub const PERMANENT_REDIRECT: u16 = 308;
pub const BAD_REQUEST: u16 = 400;
pub const UNAUTHORIZED: u16 = 401;
pub const PAYMENT_REQUIRED: u16 = 402;
pub const FORBIDDEN: u16 = 403;
pub const NOT_FOUND: u16 = 404;
pub const REQUEST_TIMEOUT: u16 = 408;
pub const CONFLICT: u16 = 409;
pub const PAYLOAD_TOO_LARGE: u16 = 413;
pub const TOO_MANY_REQUESTS: u16 = 429;
pub const INTERNAL_SERVER_ERROR: u16 = 500;
pub const GATEWAY_TIMEOUT: u16 = 504;

/// 2xx：成功。
pub const SUCCESS: Range<u16> = OK..300;
/// 4xx：客户端错误。
pub const CLIENT_ERROR: Range<u16> = BAD_REQUEST..INTERNAL_SERVER_ERROR;
/// 5xx 起：服务端错误（不设上限，非标准的 5xx 也算）。
pub const SERVER_ERROR_MIN: u16 = INTERNAL_SERVER_ERROR;

/// 跟随 `Location` 的重定向码（304 不算）。
pub const REDIRECTS: [u16; 5] = [
    MOVED_PERMANENTLY,
    FOUND,
    SEE_OTHER,
    TEMPORARY_REDIRECT,
    PERMANENT_REDIRECT,
];

/// 状态码是 2xx。
pub fn is_success(status: u16) -> bool {
    SUCCESS.contains(&status)
}

/// 状态码是 4xx。
pub fn is_client_error(status: u16) -> bool {
    CLIENT_ERROR.contains(&status)
}

/// 状态码是要跟随的重定向。
pub fn is_redirect(status: u16) -> bool {
    REDIRECTS.contains(&status)
}

/// 常见状态码的原因短语（WebFetch 给模型看的错误文字用它，表里没有的为空串）。
pub const REASON_PHRASES: &[(u16, &str)] = &[
    (200, "OK"),
    (201, "Created"),
    (202, "Accepted"),
    (203, "Non-Authoritative Information"),
    (204, "No Content"),
    (206, "Partial Content"),
    (301, "Moved Permanently"),
    (302, "Found"),
    (303, "See Other"),
    (304, "Not Modified"),
    (307, "Temporary Redirect"),
    (308, "Permanent Redirect"),
    (400, "Bad Request"),
    (401, "Unauthorized"),
    (402, "Payment Required"),
    (403, "Forbidden"),
    (404, "Not Found"),
    (405, "Method Not Allowed"),
    (406, "Not Acceptable"),
    (408, "Request Timeout"),
    (409, "Conflict"),
    (410, "Gone"),
    (413, "Payload Too Large"),
    (415, "Unsupported Media Type"),
    (418, "I'm a Teapot"),
    (422, "Unprocessable Entity"),
    (429, "Too Many Requests"),
    (451, "Unavailable For Legal Reasons"),
    (500, "Internal Server Error"),
    (501, "Not Implemented"),
    (502, "Bad Gateway"),
    (503, "Service Unavailable"),
    (504, "Gateway Timeout"),
];

/// 状态码的原因短语；表里没有时为空串。
pub fn reason_phrase(status: u16) -> &'static str {
    REASON_PHRASES
        .iter()
        .find(|(code, _)| *code == status)
        .map_or("", |(_, phrase)| phrase)
}

/// 通用的请求头名（小写：发送时按 `http::HeaderName` 规范化成小写，写法不影响线上的字节）。
pub mod header {
    pub const ACCEPT: &str = "accept";
    pub const AUTHORIZATION: &str = "authorization";
    pub const CONTENT_TYPE: &str = "content-type";
    pub const RETRY_AFTER: &str = "retry-after";
    pub const USER_AGENT: &str = "user-agent";
}
