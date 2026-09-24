//! Runtime environment and network egress for every outbound HTTP request and
//! tool child process (Node `shared/src/runtimeEnv.ts`, `adapters/src/network`,
//! `bootstrap/src/model-config.ts`). This crate has no internal dependencies.
mod child;
pub mod device;
mod env;
pub mod gateway;
pub mod headers;
mod platform;
mod proxy;
#[cfg(test)]
mod tests;

pub use child::{NetworkPolicy, tool_env};
pub use env::{PASSTHROUGH_KEY, RuntimeEnv};
pub use headers::Headers;
pub use proxy::{ProxyResolution, ProxyRules};

use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::OnceCell;

/// What a client is used for; each purpose gets one lazily built client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    Model,
    Mcp,
    /// WebFetch GETs: shell proxies apply as a fallback (Node
    /// `resolveWebFetchProxyForRequest`), redirects are handled by the tool.
    WebFetch,
}

#[derive(Debug)]
pub enum EgressError {
    /// The configured CA bundle cannot be read or parsed.
    CaCertificate(String),
    Client(reqwest::Error),
}

impl std::fmt::Display for EgressError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CaCertificate(error) => write!(f, "Cannot load CA certificates: {error}"),
            Self::Client(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for EgressError {}

/// Single owner of the startup network policy, identity headers, device id and
/// HTTP clients. Node reads the `network` section once when the app is created;
/// changing it afterwards does not affect existing egress, so neither does Rust.
pub struct Egress {
    env: Arc<RuntimeEnv>,
    rules: Arc<ProxyRules>,
    ca_cert_file: Option<String>,
    identity: Headers,
    origin: String,
    tool_env: Arc<[(String, String)]>,
    device_file: PathBuf,
    device: OnceCell<String>,
    clients: [OnceCell<reqwest::Client>; 3],
}

impl Egress {
    /// `title` is `electron` for the app server and `cli` for terminal frontends.
    pub fn new(
        env: Arc<RuntimeEnv>,
        policy: &NetworkPolicy,
        home: &std::path::Path,
        title: &str,
    ) -> Result<Self, String> {
        let lookup = |key: &str| env.get(key);
        let origin = headers::endpoint_origin(lookup)?;
        let identity = headers::identity(lookup, title, env!("CARGO_PKG_VERSION"))?;
        // Node `resolveTlsCaCertFile`：配置值为空白时回退到环境变量。
        let ca_cert_file = [
            policy.ca_cert_file.as_deref(),
            env.get("ZCODE_AGENT_CA_CERT"),
        ]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|p| !p.is_empty())
        .map(str::to_owned);
        let cwd = std::env::current_dir().unwrap_or_default();
        Ok(Self {
            rules: Arc::new(ProxyRules::new(policy, lookup)),
            ca_cert_file,
            identity,
            origin,
            tool_env: tool_env(env.vars(), policy, cfg!(windows)).into(),
            device_file: device::state_file(env.get("ZCODE_DATA_BASE_DIR"), home, &cwd),
            device: OnceCell::new(),
            clients: Default::default(),
            env,
        })
    }

    /// Node's `process.env` after startup sanitization; used as-is by plain
    /// child processes such as git.
    pub fn runtime_env(&self) -> &RuntimeEnv {
        &self.env
    }

    /// Complete environment for tool children (Bash, MCP stdio).
    pub fn tool_env(&self) -> Arc<[(String, String)]> {
        self.tool_env.clone()
    }

    /// Identity defaults for model requests.
    pub fn identity(&self) -> &Headers {
        &self.identity
    }

    /// Coding Plan gateway URL for official endpoints.
    pub fn gateway(&self, url: &str) -> Option<String> {
        gateway::rewrite(url, &self.origin)
    }

    pub fn resolve_proxy(&self, url: &str, web_fetch: bool) -> ProxyResolution {
        self.rules.resolve(url, web_fetch)
    }

    /// Persisted device id, resolved once per process.
    pub async fn device_id(&self) -> &str {
        self.device
            .get_or_init(|| device::ensure(&self.device_file))
            .await
    }

    pub async fn client(&self, purpose: Purpose) -> Result<reqwest::Client, EgressError> {
        let cell = &self.clients[purpose as usize];
        cell.get_or_try_init(|| async {
            let rules = self.rules.clone();
            let ca = self.ca_cert_file.clone();
            // CA 文件读取与平台证书校验器初始化是阻塞 IO，不能占用运行时工作线程。
            tokio::task::spawn_blocking(move || build_client(purpose, rules, ca.as_deref()))
                .await
                .map_err(|e| EgressError::CaCertificate(e.to_string()))?
        })
        .await
        .cloned()
    }
}

fn build_client(
    purpose: Purpose,
    rules: Arc<ProxyRules>,
    ca_cert_file: Option<&str>,
) -> Result<reqwest::Client, EgressError> {
    // rustls-no-provider 不自动选择算法；所有客户端统一使用 ring。
    let _ = rustls::crypto::ring::default_provider().install_default();
    // 与 Node 一致：模型与 MCP 请求只认 ZCode 显式代理，不读取 shell 的 HTTP(S)_PROXY。
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy();
    let web_fetch = purpose == Purpose::WebFetch;
    if rules.has_explicit_proxy() || web_fetch && rules.has_web_fetch_proxy() {
        builder = builder.proxy(reqwest::Proxy::custom(move |url| {
            rules.resolve(url.as_str(), web_fetch).proxy
        }));
    }
    if let Some(path) = ca_cert_file {
        let pem = std::fs::read(path).map_err(|e| EgressError::CaCertificate(e.to_string()))?;
        let certificates = reqwest::Certificate::from_pem_bundle(&pem)
            .map_err(|e| EgressError::CaCertificate(e.to_string()))?;
        // Node 的 https.Agent({ ca }) 替换而非追加根证书。
        builder = builder.tls_certs_only(certificates);
    }
    builder = match purpose {
        Purpose::Model => builder
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_nodelay(true),
        Purpose::Mcp => builder.connect_timeout(Duration::from_secs(15)),
        // undici 的默认连接超时（Node WebFetch 的 `Connect Timeout Error` 文本依赖它）。
        Purpose::WebFetch => builder.connect_timeout(Duration::from_secs(10)),
    };
    builder.build().map_err(EgressError::Client)
}
