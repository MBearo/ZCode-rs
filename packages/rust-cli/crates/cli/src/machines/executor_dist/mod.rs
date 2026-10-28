//! 执行器的 npm 来源（F100，docs/specs/rust-cli/remote-exec.md §26）：实现 ssh-connect 的 `ArtifactSource`。远端没有能用
//! 的执行器、本机没有静态产物时，部署器经它按远端的架构拿同版本的 Linux 平台包（`@mbearo/zcode-linux-*-gnu`）：先看本机
//! 缓存 `~/.zcode/rust-cli/exec/dist/<版本>/<gnu target>/zcode-rs`，没有就从注册表下载、校验，原子地写进缓存。
//!
//! 单飞：同一个（版本，target）在一个进程里只下载一次（所有机器共用这一个来源），一起等的拿到同一个结果；完成后从表里
//! 拿掉，以后的连接重新判断。跨进程靠“先写临时文件再改名”：读的一方只会看到完整的文件或没有。不用超时去对齐状态。
//!
//! F103（§27.3）：失败分两种——这个版本不在注册表里（packument 404、`versions` 里没有、tarball 404；带上注册表的
//! latest），部署器可以退到 latest；别的失败（网络、校验、写缓存）不退。退到 latest 时部署器按 latest 的实际版本再要
//! 一次，所以缓存目录与单飞的键都是实际版本。

mod package;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use futures_util::FutureExt;
use futures_util::future::{BoxFuture, Shared};
use rust_cli_fs::{FileMode, ParentDir, TempName, WriteOptions};
use rust_cli_net::{Egress, HttpRequest};
use rust_cli_paths::ExecutorPaths;
use rust_cli_ssh_connect::{ArtifactSource, DownloadError, Platform};

use crate::options::TestDeclarations;
use crate::update::registry as npm;

/// tarball 最多这么大（执行器的包压缩后几十 MB）。
const MAX_TARBALL: usize = 256 * 1024 * 1024;
/// 下载 tarball 时两块数据之间最多等这么久（存活的上限，不用来同步状态）。
const CHUNK_IDLE: Duration = Duration::from_secs(30);

/// 下载来源的注册表（§26.7）：release 构建总是官方注册表；debug 构建的版本 npm 上没有，只在给了只给测试的假注册表
/// （`_ZCODE_ASSUME_NPM_REGISTRY`）时下载。`declarations` 是本次构建是否接受测试声明（release 忽略）。
pub(crate) fn registry(declarations: TestDeclarations, assumed: Option<String>) -> Option<String> {
    match declarations {
        TestDeclarations::Ignored => Some(npm::REGISTRY.to_owned()),
        TestDeclarations::Accepted => assumed,
    }
}

/// 组装根用：有注册表时给一个来源（一个进程一个，所有机器共用）。
pub(crate) fn source(
    registry: Option<String>,
    egress: &Arc<Egress>,
    executor: &ExecutorPaths,
) -> Option<Arc<dyn ArtifactSource>> {
    let registry = registry?;
    let source: Arc<dyn ArtifactSource> = Arc::new(NpmExecutors::new(
        egress.clone(),
        registry,
        executor.clone(),
    ));
    Some(source)
}

type Flight = Shared<BoxFuture<'static, Result<PathBuf, DownloadError>>>;

pub(crate) struct NpmExecutors {
    fetcher: Arc<Fetcher>,
    /// 正在下载的（版本/target → 这一次下载）。
    flights: Mutex<HashMap<String, Flight>>,
}

struct Fetcher {
    egress: Arc<Egress>,
    registry: String,
    executor: ExecutorPaths,
}

impl NpmExecutors {
    pub(crate) fn new(egress: Arc<Egress>, registry: String, executor: ExecutorPaths) -> Self {
        Self {
            fetcher: Arc::new(Fetcher {
                egress,
                registry,
                executor,
            }),
            flights: Mutex::new(HashMap::new()),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[async_trait]
impl ArtifactSource for NpmExecutors {
    async fn cached(&self, version: &str, platform: Platform) -> Option<PathBuf> {
        self.fetcher.cached(version, platform).await
    }

    async fn download(&self, version: &str, platform: Platform) -> Result<PathBuf, DownloadError> {
        let key = format!("{version}/{}", platform.triple());
        let flight = lock(&self.flights)
            .entry(key.clone())
            .or_insert_with(|| {
                let fetcher = self.fetcher.clone();
                let version = version.to_owned();
                async move { fetcher.fetch(&version, platform).await }
                    .boxed()
                    .shared()
            })
            .clone();
        let result = flight.clone().await;
        // 完成后拿掉（还是这一次的才拿）：以后的连接重新判断（缓存命中或再下载）。
        let mut flights = lock(&self.flights);
        if flights
            .get(&key)
            .is_some_and(|current| current.ptr_eq(&flight))
        {
            flights.remove(&key);
        }
        result
    }
}

impl Fetcher {
    fn path(&self, version: &str, platform: Platform) -> PathBuf {
        self.executor
            .artifact(version, platform.triple())
            .as_path()
            .to_path_buf()
    }

    async fn cached(&self, version: &str, platform: Platform) -> Option<PathBuf> {
        let path = self.path(version, platform);
        matches!(rust_cli_fs::file_len(&path).await, Ok(Some(_))).then_some(path)
    }

    /// 下载、校验、原子写进缓存（§26.4）。别的进程或别的机器刚下载完时直接用缓存。
    async fn fetch(&self, version: &str, platform: Platform) -> Result<PathBuf, DownloadError> {
        if let Some(path) = self.cached(version, platform).await {
            return Ok(path);
        }
        // F107（§28.2）：这个平台没有发布的包（Intel Mac），一个请求都不发（部署器本来也不会走到这里）。
        if !platform.published() {
            return Err(DownloadError::Failed(format!(
                "there is no zcode executor package for {}",
                platform.triple()
            )));
        }
        let package = package::package_name(platform);
        let url = npm::package_url(&self.registry, &package);
        tracing::info!(target: "executor_dist", %package, %version, %url, "downloading the executor");
        let result = self.download(&package, version, platform, &url).await;
        match &result {
            Ok(path) => {
                tracing::info!(target: "executor_dist", %package, %version, path = %path.display(), "executor downloaded");
            }
            // 本地 release 编译的版本本来就不在注册表里（F103）：不算异常。
            Err(DownloadError::Unpublished { reason, latest }) => {
                tracing::info!(target: "executor_dist", %package, %version, %reason, latest = latest.as_deref().unwrap_or("none"), "this executor version is not on the registry");
            }
            Err(DownloadError::Failed(reason)) => {
                tracing::warn!(target: "executor_dist", %package, %version, error = %reason, "executor download failed");
            }
        }
        result
    }

    /// 取 packument 与 tarball；“这个版本不在注册表里”与别的失败分开（F103，§27.3）。
    async fn download(
        &self,
        package: &str,
        version: &str,
        platform: Platform,
        url: &str,
    ) -> Result<PathBuf, DownloadError> {
        let failed = |error: anyhow::Error| DownloadError::Failed(format!("{error:#}"));
        let document = npm::fetch(&self.egress, url)
            .await
            .map_err(|error| missing_or_failed(error, None))?;
        let latest = npm::latest_tag(&document)
            .ok()
            .filter(|latest| package::plain_version(latest))
            .map(str::to_owned);
        let listed = document
            .get("versions")
            .and_then(|versions| versions.get(version))
            .is_some();
        if let Err(error) = npm::ensure_dist(&document, package, version) {
            return Err(if listed {
                failed(error)
            } else {
                DownloadError::Unpublished {
                    reason: format!("{error:#}"),
                    latest,
                }
            });
        }
        let dist = &document["versions"][version]["dist"];
        let tarball = dist["tarball"].as_str().unwrap_or_default();
        let integrity = dist["integrity"].as_str().unwrap_or_default();
        // 注册表是 https 时 tarball 也必须是（测试的假注册表是本机的 http）。
        if !tarball.starts_with("https://") && !self.registry.starts_with("http://") {
            return Err(DownloadError::Failed(format!(
                "{package} {version}: the tarball is not https: {tarball}"
            )));
        }
        let bytes = get_bytes(&self.egress, tarball)
            .await
            .map_err(|error| missing_or_failed(error, latest))?;
        self.store(package, (version, platform), bytes, integrity)
            .await
            .map_err(failed)
    }

    /// 核对 integrity、解出执行器并核对，原子写进缓存（§26.4）。
    async fn store(
        &self,
        package: &str,
        (version, platform): (&str, Platform),
        bytes: Vec<u8>,
        integrity: &str,
    ) -> Result<PathBuf> {
        if !package::integrity_matches(&bytes, integrity) {
            bail!(
                "{package} {version}: the download does not match the registry's sha512 integrity"
            );
        }
        let owned = version.to_owned();
        let binary = rust_cli_base::blocking(move || package::extract(&bytes, &owned, platform))
            .await
            .map_err(|error| anyhow!("{package} {version}: {error}"))?
            .map_err(|error| anyhow!("{package} {version}: {error}"))?;
        let path = self.path(version, platform);
        let dir = path
            .parent()
            .ok_or_else(|| anyhow!("{} has no parent", path.display()))?;
        rust_cli_fs::create_private_dir_all(dir)
            .await
            .with_context(|| format!("creating {}", dir.display()))?;
        let options = WriteOptions {
            mode: FileMode::Executable,
            temp_name: TempName::Hidden,
            parent: ParentDir::Existing,
            rename_retry: &[],
        };
        rust_cli_fs::atomic_write(&path, &binary, options)
            .await
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(path)
    }
}

/// 注册表回 404（包或文件不在）：这个版本不在注册表里，带上 `latest`；别的错误是 `Failed`（F103，§27.3）。
fn missing_or_failed(error: anyhow::Error, latest: Option<String>) -> DownloadError {
    let reason = format!("{error:#}");
    if npm::HttpStatus::is_not_found(&error) {
        DownloadError::Unpublished { reason, latest }
    } else {
        DownloadError::Failed(reason)
    }
}

/// GET 一个文件的全部内容：响应头最多等 `timeout::NPM_REGISTRY`，两块之间最多等 [`CHUNK_IDLE`]，最多 [`MAX_TARBALL`]。
async fn get_bytes(egress: &Egress, url: &str) -> Result<Vec<u8>> {
    let mut request = HttpRequest::get(url);
    request.timeout = Some(rust_cli_net::timeout::NPM_REGISTRY);
    let mut response = egress
        .send(request)
        .await
        .with_context(|| format!("GET {url}"))?;
    if !response.is_success() {
        return Err(npm::HttpStatus {
            url: url.to_owned(),
            status: response.status(),
        }
        .into());
    }
    let mut body = Vec::new();
    loop {
        let chunk = tokio::time::timeout(CHUNK_IDLE, response.chunk())
            .await
            .map_err(|_| anyhow!("GET {url}: no data for {} s", CHUNK_IDLE.as_secs()))?
            .with_context(|| format!("reading {url}"))?;
        let Some(chunk) = chunk else { break };
        if body.len() + chunk.len() > MAX_TARBALL {
            bail!("GET {url}: the response is larger than {MAX_TARBALL} bytes");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
#[path = "fake_npm_tests.rs"]
mod fake_npm_tests;
#[cfg(test)]
#[path = "latest_tests.rs"]
mod latest_tests;
#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
