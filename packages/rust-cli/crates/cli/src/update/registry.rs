//! 取最新版本（docs/specs/rust-cli/update.md §5，Codex `npm_registry.rs`）：入口包的 `dist-tags.latest`，核对入口包与
//! 平台包的这个版本在 npm 上能装（`dist.tarball` 与 `dist.integrity` 都有）。HTTP 只经 net 的 `Egress`。

use anyhow::{Context, Result, anyhow, bail};
use rust_cli_net::{Egress, HttpRequest};
use serde_json::Value;

use super::install::Install;

/// npm 官方 registry。
pub(crate) const REGISTRY: &str = "https://registry.npmjs.org";
/// npm 的缩略元数据（只有 dist-tags 与每个版本的安装信息），没有时退回完整文档。
const ACCEPT: &str = "application/vnd.npm.install-v1+json; q=1.0, application/json; q=0.8, */*";
/// 响应体最多读这么多。
const MAX_BODY: usize = 32 * 1024 * 1024;

/// 注册表回了不成功的 HTTP 状态（文字同以前的 `GET <地址>: HTTP <状态>`）。执行器的 npm 来源（F103，remote-exec.md
/// §27.3）按 404 判定“这个版本不在注册表里”，所以给出类型、不只是文字。
#[derive(Debug)]
pub(crate) struct HttpStatus {
    pub(crate) url: String,
    pub(crate) status: u16,
}

impl std::fmt::Display for HttpStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GET {}: HTTP {}", self.url, self.status)
    }
}

impl std::error::Error for HttpStatus {}

impl HttpStatus {
    /// `error` 是不是注册表回的 404（没有这个包或文件）。
    pub(crate) fn is_not_found(error: &anyhow::Error) -> bool {
        error
            .downcast_ref::<Self>()
            .is_some_and(|status| status.status == 404)
    }
}

/// 包元数据的地址：作用域包的 `/` 写成 `%2f`（Codex `@openai%2fcodex`）。
pub(crate) fn package_url(registry: &str, package: &str) -> String {
    format!(
        "{}/{}",
        registry.trim_end_matches('/'),
        package.replace('/', "%2f")
    )
}

/// 入口包的最新版本，核对入口包（与启动器给的平台包）都能装。
pub(crate) async fn latest(egress: &Egress, registry: &str, install: &Install) -> Result<String> {
    let entry = fetch(egress, &package_url(registry, &install.package)).await?;
    let latest = latest_tag(&entry)?.to_owned();
    ensure_dist(&entry, &install.package, &latest)?;
    if let Some(platform) = &install.platform_package {
        let document = fetch(egress, &package_url(registry, platform)).await?;
        ensure_dist(&document, platform, &latest)?;
    }
    Ok(latest)
}

/// `dist-tags.latest`。
pub(crate) fn latest_tag(document: &Value) -> Result<&str> {
    document
        .get("dist-tags")
        .and_then(|tags| tags.get("latest"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|latest| !latest.is_empty())
        .ok_or_else(|| anyhow!("npm package is missing latest dist-tag"))
}

/// `versions[version].dist` 的 `tarball` 与 `integrity` 都是非空字符串（Codex `version_info_with_dist`）。
pub(crate) fn ensure_dist(document: &Value, package: &str, version: &str) -> Result<()> {
    let info = document
        .get("versions")
        .and_then(|versions| versions.get(version))
        .ok_or_else(|| anyhow!("npm package {package} version {version} is missing"))?;
    let Some(dist) = info.get("dist").filter(|dist| dist.is_object()) else {
        bail!("npm package {package} version {version} is missing dist metadata");
    };
    for field in ["tarball", "integrity"] {
        let present = dist
            .get(field)
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty());
        if !present {
            bail!("npm package {package} version {version} is missing dist.{field}");
        }
    }
    Ok(())
}

/// 取一个包的元数据（缩略的 install-v1 文档）。
pub(crate) async fn fetch(egress: &Egress, url: &str) -> Result<Value> {
    let mut request = HttpRequest::get(url).header(rust_cli_net::header::ACCEPT, ACCEPT);
    // 修复线 F58：每个请求最多等 5 s 的响应头（net `timeout::NPM_REGISTRY`）；以前没有单次超时，后台检查可能一直挂着。
    request.timeout = Some(rust_cli_net::timeout::NPM_REGISTRY);
    let mut response = egress
        .send(request)
        .await
        .with_context(|| format!("GET {url}"))?;
    if !response.is_success() {
        return Err(HttpStatus {
            url: url.to_owned(),
            status: response.status(),
        }
        .into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .with_context(|| format!("reading {url}"))?
    {
        if body.len() + chunk.len() > MAX_BODY {
            bail!("GET {url}: the response is larger than {MAX_BODY} bytes");
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).with_context(|| format!("GET {url}: not JSON"))
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
