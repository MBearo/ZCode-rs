//! 网络出口的来源（docs/specs/rust-cli/config.md §2.4，修复线 F21）：入口从环境快照算出代理地址、不走代理的
//! 规则与信任的根证书，交给 net 的 `Egress`。CA 文件经 rust-cli-fs 读。
//!
//! 修复（审查 R2-2）：以前入口一直用 `NetworkPolicy::default`，不读任何代理与 CA，企业网络连不上（子进程却经
//! 白名单拿到 `HTTPS_PROXY`）。现在 Node 版 zcode 的 `ZCODE_*` 变量逐项优先（Node 的语义），没有时读标准变量。

use std::fmt;
use std::io;
use std::path::Path;

use anyhow::{Result, anyhow};
use rust_cli_base::js::string::trim;
use rust_cli_base::secret::EnvSnapshot;
use rust_cli_net::{CaBundle, NetworkPolicy, NoProxy, TrustRoots};

/// Node 版 zcode 的根证书变量（读值、报错与日志都用这个名字，F55：以前写了三次）。
const AGENT_CA_CERT: &str = "ZCODE_AGENT_CA_CERT";

/// 按这个顺序取第一个非空的代理变量。
const STANDARD_PROXY: [&str; 4] = ["https_proxy", "HTTPS_PROXY", "http_proxy", "HTTP_PROXY"];

/// 标准代理变量不是带主机的完整 URL（启动时的用户错误）。入口把它原样写到 stderr，退出码 1。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InvalidProxy {
    source: &'static str,
    value: String,
}

impl fmt::Display for InvalidProxy {
    /// 固定的文字；值去掉 Unicode 控制字符（`/\p{Cc}/gu`）。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let source = self.source;
        let value: String = self.value.chars().filter(|c| !c.is_control()).collect();
        write!(
            formatter,
            "Invalid proxy URL in {source}: \"{value}\" cannot be parsed as a URL.\n\
             Proxy settings must be a complete URL including the scheme, e.g. \"http://proxy.example.com:8080\".\n\
             Fix or unset {source} and restart ZCode."
        )
    }
}

impl std::error::Error for InvalidProxy {}

/// 出口策略，以及启动时要写到 stderr 的警告（`NODE_EXTRA_CA_CERTS` 用不了时）。
pub(crate) struct Network {
    pub(crate) policy: NetworkPolicy,
    pub(crate) warnings: Vec<String>,
}

/// 按环境快照算出口策略。`cwd` 是 CA 文件相对路径的基准（与 Node 的 `readFileSync` 相同，相对进程 cwd）。
pub(crate) async fn network(env: &EnvSnapshot, cwd: &Path) -> Result<Network> {
    let proxy = proxy(env)?;
    let mut warnings = Vec::new();
    let roots = match ca_source(env) {
        None => TrustRoots::Platform,
        Some(CaSource::Agent(value)) => agent_roots(value, cwd).await?,
        Some(CaSource::Extra(value)) => extra_roots(value, cwd, &mut warnings).await,
    };
    Ok(Network {
        policy: NetworkPolicy {
            proxy,
            no_proxy: no_proxy(env),
            roots,
            ..NetworkPolicy::default()
        },
        warnings,
    })
}

/// 按名字取变量。Windows 的变量名不分大小写（Node 与 Bun 的 `process.env` 相同；F58 起是快照唯一的规则）。
fn var<'a>(env: &'a EnvSnapshot, name: &str) -> Option<&'a str> {
    env.get(name)
}

/// 代理地址：`ZCODE_HTTP_PROXY`（Node 的写法）有效时用它，否则取第一个非空的标准变量。
pub(crate) fn proxy(env: &EnvSnapshot) -> Result<Option<String>, InvalidProxy> {
    if let Some(url) = var(env, "ZCODE_HTTP_PROXY").and_then(node_proxy_url) {
        return Ok(Some(url));
    }
    let Some((source, value)) = STANDARD_PROXY.iter().find_map(|name| {
        var(env, name)
            .filter(|value| !value.is_empty())
            .map(|value| (*name, value))
    }) else {
        return Ok(None);
    };
    // `new URL(value)` 能解析且有主机。值原样（不去空白）。
    match url::Url::parse(value) {
        Ok(url) if url.host_str().is_some_and(|host| !host.is_empty()) => Ok(Some(url.into())),
        _ => Err(InvalidProxy {
            source,
            value: value.to_owned(),
        }),
    }
}

/// Node `normalizeProxyUrl`：去掉首尾空白，空等于没有；没有 `scheme://` 时补 `http://`；解析不了当作没有。
fn node_proxy_url(value: &str) -> Option<String> {
    let trimmed = trim(value);
    if trimmed.is_empty() {
        return None;
    }
    let candidate = if has_scheme(trimmed) {
        trimmed.to_owned()
    } else {
        format!("http://{trimmed}")
    };
    url::Url::parse(&candidate).ok().map(String::from)
}

/// `/^[a-z][a-z0-9+.-]*:\/\//i`。
fn has_scheme(value: &str) -> bool {
    let Some((scheme, _)) = value.split_once("://") else {
        return false;
    };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
}

/// 不走代理的规则：`ZCODE_NO_PROXY`（去空白后非空）优先，否则是合并后的 `no_proxy` / `NO_PROXY`。
pub(crate) fn no_proxy(env: &EnvSnapshot) -> NoProxy {
    if let Some(value) = var(env, "ZCODE_NO_PROXY")
        .map(trim)
        .filter(|value| !value.is_empty())
    {
        return NoProxy::Node(value.to_owned());
    }
    let lower = var(env, "no_proxy").unwrap_or("");
    let upper = var(env, "NO_PROXY").unwrap_or("");
    let merged = if lower == "*" || upper == "*" {
        "*".to_owned()
    } else if !lower.is_empty() && !upper.is_empty() && lower != upper {
        format!("{lower},{upper}")
    } else if lower.is_empty() {
        upper.to_owned()
    } else {
        lower.to_owned()
    };
    if merged.is_empty() {
        NoProxy::None
    } else {
        NoProxy::Standard(merged)
    }
}

/// CA 文件的来源；值是变量里写的路径（报错与警告里原样显示）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CaSource {
    /// `ZCODE_AGENT_CA_CERT`（去空白后非空）：替换平台根证书。
    Agent(String),
    /// `NODE_EXTRA_CA_CERTS`（非空）：加在平台根证书上。
    Extra(String),
}

pub(crate) fn ca_source(env: &EnvSnapshot) -> Option<CaSource> {
    if let Some(path) = var(env, AGENT_CA_CERT)
        .map(trim)
        .filter(|path| !path.is_empty())
    {
        return Some(CaSource::Agent(path.to_owned()));
    }
    var(env, "NODE_EXTRA_CA_CERTS")
        .filter(|path| !path.is_empty())
        .map(|path| CaSource::Extra(path.to_owned()))
}

/// `ZCODE_AGENT_CA_CERT`：读不出时报错退出（config.md D14）；没有可用证书时照常，所有 HTTPS 请求都在证书
/// 校验时失败（Node 的 `ca` 相同），运行日志记一条警告。
async fn agent_roots(value: String, cwd: &Path) -> Result<TrustRoots> {
    let bytes = read(&cwd.join(&value))
        .await
        .map_err(|reason| anyhow!("{AGENT_CA_CERT}: cannot read {value}: {reason}"))?;
    let bundle = CaBundle::from_pem(&bytes);
    if let Some(problem) = bundle.problem() {
        tracing::warn!(
            event = "network.ca",
            source = AGENT_CA_CERT,
            certificates = bundle.len(),
            problem,
            "CA file has unusable content; HTTPS requests trust only the certificates read before it"
        );
    }
    Ok(TrustRoots::Only(bundle))
}

/// `NODE_EXTRA_CA_CERTS`：读不出、遇到坏块或没有证书时写一行警告，照常启动；
/// 坏块之前的证书照用。
async fn extra_roots(value: String, cwd: &Path, warnings: &mut Vec<String>) -> TrustRoots {
    let bundle = match read(&cwd.join(&value)).await {
        Ok(bytes) => CaBundle::from_pem(&bytes),
        Err(reason) => {
            warnings.push(warning(&value, &reason));
            return TrustRoots::Platform;
        }
    };
    if let Some(problem) = bundle.problem() {
        warnings.push(warning(&value, problem));
    }
    if bundle.is_empty() {
        TrustRoots::Platform
    } else {
        TrustRoots::Extra(bundle)
    }
}

fn warning(path: &str, reason: &str) -> String {
    format!("warn: ignoring extra certs from {path}, load failed: {reason}\n")
}

/// 读 CA 文件。失败时给系统的说明（Node 与 Bun 写的就是它，不带 Rust 的 ` (os error N)`）。
async fn read(path: &Path) -> Result<Vec<u8>, String> {
    match rust_cli_fs::read_optional(path).await {
        Ok(Some(bytes)) => Ok(bytes),
        Ok(None) => Err("No such file or directory".to_owned()),
        Err(error) => Err(os_message(&error)),
    }
}

fn os_message(error: &io::Error) -> String {
    let text = error.to_string();
    match text.rfind(" (os error ") {
        Some(at) if text.ends_with(')') => text[..at].to_owned(),
        _ => text,
    }
}

#[cfg(test)]
#[path = "network_tests.rs"]
mod tests;
