//! 出口策略与 HTTP 客户端（docs/specs/rust-cli/model.md §2.1）。
//!
//! 策略与按它构造的客户端组成一份快照，热替换时整份发布（R4）。每个请求开始时读一次快照，
//! 正在进行的请求不受替换影响。替换按调用顺序生效：后发起的替换总是覆盖先发起的，即使先发起
//! 的那次构造得更慢。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use reqwest::header::HeaderMap;
use rust_cli_base::redact::redact_secrets;

use crate::http::header_pair;
use crate::roots::{Anchors, certificates};
use crate::{HeaderValue, HttpRequest, HttpResponse, NetError, NoProxy, TrustRoots};

/// reqwest 认得的代理协议。别的协议它会静默直连（hyper-util 的 matcher 不拦截），所以构造时拒绝。
const PROXY_SCHEMES: [&str; 6] = ["http", "https", "socks4", "socks4a", "socks5", "socks5h"];
/// 未配置建连超时时的默认值。
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// 空闲连接保留时间（与 apps/zcode-cli-rust 的模型客户端相同）。
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

/// 出口策略。热更新时整份替换，请求开始时读一次。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetworkPolicy {
    /// 附加到每个请求的 header（名、值）；请求自己设置的同名 header 优先。
    pub headers: Vec<(String, String)>,
    /// 代理（`http`、`https`、`socks4`、`socks4a`、`socks5`、`socks5h`）；为空时直连。net 从不读系统代理与
    /// 环境变量：地址由入口按环境变量算出（docs/specs/rust-cli/config.md §2.4）。
    pub proxy: Option<String>,
    /// 不走代理的目标（`proxy` 为空时不用）。
    pub no_proxy: NoProxy,
    /// 信任的根证书。读 CA 文件是入口的事。
    pub roots: TrustRoots,
    /// 建连超时；为空时用默认值（30 秒）。
    pub connect_timeout_ms: Option<u64>,
}

struct Snapshot {
    generation: u64,
    policy: Arc<NetworkPolicy>,
    headers: HeaderMap,
    client: reqwest::Client,
}

impl Snapshot {
    /// 读取平台根证书、解析 CA 可能阻塞，放到阻塞线程池里做。
    async fn build(policy: NetworkPolicy, generation: u64) -> Result<Self, NetError> {
        rust_cli_base::blocking(move || {
            let headers = policy_headers(&policy)?;
            let client = client(&policy)?;
            Ok(Self {
                generation,
                policy: Arc::new(policy),
                headers,
                client,
            })
        })
        .await
        .map_err(|error| NetError::Policy(error.to_string()))?
    }
}

/// 网络出口。所有出站 HTTP 都经它发出。
pub struct Egress {
    current: ArcSwap<Snapshot>,
    /// 最近一次发起替换的序号；快照只被序号更大的替换覆盖。
    issued: AtomicU64,
}

impl Egress {
    pub async fn new(policy: NetworkPolicy) -> Result<Self, NetError> {
        let snapshot = Snapshot::build(policy, 0).await?;
        Ok(Self {
            current: ArcSwap::from_pointee(snapshot),
            issued: AtomicU64::new(0),
        })
    }

    /// 整份替换策略。新策略无效时返回错误，旧策略继续生效。
    pub async fn replace(&self, policy: NetworkPolicy) -> Result<(), NetError> {
        let generation = self.issued.fetch_add(1, Ordering::AcqRel) + 1;
        let fresh = Arc::new(Snapshot::build(policy, generation).await?);
        self.current.rcu(|current| {
            if current.generation < generation {
                fresh.clone()
            } else {
                current.clone()
            }
        });
        Ok(())
    }

    pub fn policy(&self) -> Arc<NetworkPolicy> {
        self.current.load().policy.clone()
    }

    /// 发送请求，返回响应头已到达的响应。非 2xx 状态也是 `Ok`，由调用方分类。
    pub async fn send(&self, request: HttpRequest) -> Result<HttpResponse, NetError> {
        let snapshot = self.current.load_full();
        let mut headers = snapshot.headers.clone();
        for (name, value) in &request.headers {
            let (name, value) = header_pair(name, value).map_err(NetError::Request)?;
            headers.insert(name, value);
        }
        let url = redact_secrets(&request.url).into_owned();
        let method = request.method.as_str();
        let started = Instant::now();
        let pending = snapshot
            .client
            .request(request.method.to_reqwest(), &request.url)
            .headers(headers)
            .body(request.body)
            .send();
        let outcome = match request.timeout {
            Some(limit) => match tokio::time::timeout(limit, pending).await {
                Ok(sent) => sent.map_err(|error| NetError::from_reqwest(&error)),
                Err(_) => Err(NetError::Timeout),
            },
            None => pending
                .await
                .map_err(|error| NetError::from_reqwest(&error)),
        }
        .map(HttpResponse::new);
        let elapsed_ms = started.elapsed().as_millis();
        match &outcome {
            Ok(response) => {
                tracing::debug!(method, url, status = response.status(), elapsed_ms, "http");
            }
            Err(error) => tracing::debug!(method, url, %error, elapsed_ms, "http failed"),
        }
        outcome
    }
}

impl Egress {
    /// 开一个 WebSocket（Monitor 的 ws 来源，docs/specs/rust-cli/tools-more.md §19.7）：升级请求经当前快照的客户端，
    /// 带策略的 header。
    pub async fn websocket(
        &self,
        url: &str,
        protocols: &[String],
    ) -> Result<crate::websocket::WebSocket, crate::websocket::WsError> {
        let snapshot = self.current.load_full();
        crate::websocket::connect(&snapshot.client, snapshot.headers.clone(), url, protocols).await
    }
}

/// 代理与直连规则：每个请求按目标判断（`Proxy::custom`），命中直连规则时不经代理。代理 URL 可能带用户名与
/// 密码（用作代理的 Basic 认证），错误文本里不回显它。
fn route(proxy: &str, no_proxy: NoProxy) -> Result<reqwest::Proxy, NetError> {
    let url = reqwest::Url::parse(proxy)
        .ok()
        .filter(|url| url.host_str().is_some_and(|host| !host.is_empty()))
        .ok_or_else(|| NetError::Policy("invalid proxy URL".to_owned()))?;
    if !PROXY_SCHEMES.contains(&url.scheme()) {
        return Err(NetError::Policy(format!(
            "unsupported proxy scheme {}",
            url.scheme()
        )));
    }
    Ok(reqwest::Proxy::custom(move |target| {
        (!no_proxy.bypasses(target)).then(|| url.clone())
    }))
}

fn policy_headers(policy: &NetworkPolicy) -> Result<HeaderMap, NetError> {
    let mut headers = HeaderMap::new();
    for (name, value) in &policy.headers {
        let (name, value) = header_pair(name, &HeaderValue::Plain(value.clone()))
            .map_err(|what| NetError::Policy(format!("invalid {what}")))?;
        headers.insert(name, value);
    }
    Ok(headers)
}

fn client(policy: &NetworkPolicy) -> Result<reqwest::Client, NetError> {
    build_client(policy, true)
}

/// `platform` 为假时不要平台根证书，只留附加的信任锚：只在测试里用，模拟系统根证书为空的环境（没有 ca-certificates
/// 的 Linux 镜像，平台校验器在那里只剩附加锚，egress_tests.rs）。
#[expect(
    clippy::disallowed_methods,
    reason = "rust-cli-net 是 HTTP 客户端的唯一构造点（docs/specs/rust-cli/README.md §4）"
)]
fn build_client(policy: &NetworkPolicy, platform: bool) -> Result<reqwest::Client, NetError> {
    // rustls-no-provider 不自带算法：统一装 ring。进程里已装过时返回 Err，忽略即可。
    let _ = rustls::crypto::ring::default_provider().install_default();
    let connect_timeout = policy
        .connect_timeout_ms
        .map_or(DEFAULT_CONNECT_TIMEOUT, Duration::from_millis);
    // no_proxy 同时关闭系统代理与 reqwest 自己读的 HTTP(S)_PROXY；之后只加入口算好的代理。重定向交给调用方处理。
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(connect_timeout)
        .pool_idle_timeout(POOL_IDLE_TIMEOUT)
        .tcp_nodelay(true);
    if let Some(proxy) = &policy.proxy {
        builder = builder.proxy(route(proxy, policy.no_proxy.clone())?);
    }
    builder = match policy.roots.anchors() {
        Anchors::Merge(ders) if platform => builder.tls_certs_merge(certificates(&ders)?),
        Anchors::Merge(ders) | Anchors::Only(ders) => builder.tls_certs_only(certificates(&ders)?),
    };
    builder
        .build()
        .map_err(|error| NetError::Policy(NetError::from_reqwest(&error).to_string()))
}

#[cfg(test)]
#[path = "egress_tests.rs"]
mod tests;
