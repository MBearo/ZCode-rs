//! 出口错误。错误文本都经过按值脱敏：reqwest 的错误里带完整 URL，URL 的查询串可能含 token。

use std::error::Error;

use rust_cli_base::redact::redact_secrets;

/// 一次出口操作失败的原因。上层按类型决定是否重试，不解析文本。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NetError {
    /// 出口策略无效（代理 URL、CA、header），客户端没有构造出来。
    #[error("invalid network policy: {0}")]
    Policy(String),
    /// 请求本身无效（URL、header），没有发出。
    #[error("invalid request: {0}")]
    Request(String),
    /// 建连失败（DNS、拒绝连接、代理不可达）。
    #[error("connection failed: {0}")]
    Connect(String),
    /// 建连或等待响应头超时。
    #[error("request timed out")]
    Timeout,
    /// TLS 握手或证书校验失败。重试也不会成功。
    #[error("TLS error: {0}")]
    Tls(String),
    /// 连接建立之后的传输错误（读写中断、响应体被截断）。
    #[error("network error: {0}")]
    Io(String),
}

impl NetError {
    /// 临时性的传输错误：重试可能成功。
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Connect(_) | Self::Timeout | Self::Io(_))
    }

    pub(crate) fn from_reqwest(error: &reqwest::Error) -> Self {
        let message = redact_secrets(&chain(error)).into_owned();
        if is_tls_failure(error) {
            Self::Tls(message)
        } else if error.is_builder() {
            Self::Request(message)
        } else if error.is_timeout() {
            Self::Timeout
        } else if error.is_connect() {
            Self::Connect(message)
        } else {
            Self::Io(message)
        }
    }
}

/// 错误及其全部 source，用 `: ` 连接（reqwest 的顶层文本只说"发送失败"，原因在 source 里）。
fn chain(error: &(dyn Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

// 复制自 apps/zcode-cli-rust/crates/model/src/model_failure.rs（feat/rs 79b6691c55）。
fn is_tls_failure(error: &(dyn Error + 'static)) -> bool {
    let mut source = Some(error);
    while let Some(cause) = source {
        if cause.downcast_ref::<rustls::Error>().is_some() {
            return true;
        }
        // hyper-rustls 将握手错误包成两层 io::Error；source() 会跳过内层，必须展开 get_ref。
        if let Some(inner) = cause
            .downcast_ref::<std::io::Error>()
            .and_then(|io| io.get_ref())
        {
            source = Some(inner);
        } else {
            source = cause.source();
        }
    }
    false
}
