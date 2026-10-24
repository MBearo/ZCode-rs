//! WebFetch 的出口（docs/specs/rust-cli/tools-more.md §4）：`Fetch::fetch_url` 的本地实现，请求经
//! `rust-cli-net` 的 [`Egress`] 发出。只做一次 GET：重定向与错误状态原样交回，由内核的工具判断。
//! 时限覆盖整个请求（含读正文）。

use std::time::Duration;

use rust_cli_kernel_api::env::EnvError;
use rust_cli_kernel_api::env_web::{FetchUrl, FetchedUrl};
use rust_cli_net::{Egress, HttpRequest, NetError};

fn io(error: &NetError) -> EnvError {
    EnvError::Io {
        message: error.to_string(),
    }
}

async fn send(egress: &Egress, request: FetchUrl) -> Result<FetchedUrl, EnvError> {
    let mut http = HttpRequest::get(request.url);
    for (name, value) in request.headers {
        http = http.header(name, value);
    }
    let mut response = egress.send(http).await.map_err(|error| io(&error))?;
    let header = |name: &str| response.headers().get(name).map(str::to_owned);
    let content_type = header(rust_cli_net::header::CONTENT_TYPE).unwrap_or_default();
    let location = header("location");
    let retry_after = header(rust_cli_net::header::RETRY_AFTER);
    let status = response.status();
    let limit = usize::try_from(request.max_bytes).unwrap_or(usize::MAX);
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| io(&error))? {
        if body.len() + chunk.len() > limit {
            // axios `maxContentLength` 的报错原文。
            return Err(EnvError::Io {
                message: format!("maxContentLength size of {limit} exceeded"),
            });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(FetchedUrl {
        status,
        content_type,
        location,
        retry_after,
        body,
    })
}

pub(crate) async fn fetch(egress: &Egress, request: FetchUrl) -> Result<FetchedUrl, EnvError> {
    let limit = Duration::from_millis(request.timeout_ms);
    let seconds = request.timeout_ms / 1000;
    tokio::time::timeout(limit, send(egress, request))
        .await
        .unwrap_or_else(|_| {
            Err(EnvError::Io {
                message: format!("Fetch did not complete within the {seconds}s deadline"),
            })
        })
}

#[cfg(test)]
#[path = "web_tests.rs"]
mod tests;
