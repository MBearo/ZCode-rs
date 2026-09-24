//! Verified ZIP plugin sources over HTTPS (Node `zip-source.ts`
//! `resolveZipPluginSource`, `resolveHttpZipSource`, `downloadZipArchive`,
//! `resolveZipRoot`). Spec rust-m10-4-plugin-sources §9.
use crate::http::{Get, is_redirect, resolve_location, same_origin};
use crate::plugin_source::Resolved;
use crate::ports::Ports;
use crate::zip_extract::{normalize, within};
use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value};
use sha2::Digest;
use std::path::{Path, PathBuf};
use std::time::Duration;

const DOWNLOAD_MAX_BYTES: u64 = 200 * 1024 * 1024;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_REDIRECTS: usize = 5;
const TEMP_PREFIX: &str = "zcode-plugin-zip-";
const DENIED_HEADERS: [&str; 4] = [
    "authorization",
    "cookie",
    "proxy-authorization",
    "set-cookie",
];

/// Node `PluginZipDownloadError`: a non-2xx download with its status.
#[derive(Debug)]
pub struct DownloadFailure {
    pub status: u16,
    pub message: String,
}

impl std::fmt::Display for DownloadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DownloadFailure {}

pub struct HttpZip<'a> {
    pub url: &'a str,
    pub headers: Vec<(String, String)>,
    pub path: Option<&'a str>,
    pub sha256: Option<&'a str>,
    pub strip_root: Option<bool>,
    pub require_single_root: bool,
}

fn sha_pattern(sha: &str) -> bool {
    sha.len() == 64 && sha.chars().all(|c| c.is_ascii_hexdigit())
}

/// Node `validateZipDownloadUrl`: HTTPS, or HTTP to a loopback host.
fn validate_url(value: &str) -> Result<()> {
    let url =
        url::Url::parse(value).map_err(|_| anyhow!("Plugin zip source URL is invalid: {value}"))?;
    let host = url.host_str().unwrap_or_default().to_lowercase();
    let ipv4_loopback = host.strip_prefix("127.").is_some_and(|rest| {
        rest.split('.').count() == 3 && rest.split('.').all(|p| p.parse::<u8>().is_ok())
    });
    let loopback = host == "localhost" || host == "::1" || host == "[::1]" || ipv4_loopback;
    if url.scheme() == "https" || url.scheme() == "http" && loopback {
        return Ok(());
    }
    bail!("Plugin zip source URL must be HTTPS: {value}")
}

fn validate_headers(headers: &[(String, String)]) -> Result<()> {
    if let Some((key, _)) = headers
        .iter()
        .find(|(k, _)| DENIED_HEADERS.contains(&k.to_lowercase().as_str()))
    {
        bail!("Plugin zip source header is not allowed: {key}");
    }
    Ok(())
}

/// Node `downloadZipArchive`: bounded redirects; custom headers never cross origins.
async fn download(ports: &Ports<'_>, url: &str, headers: Vec<(String, String)>) -> Result<Vec<u8>> {
    let (mut current, mut current_headers) = (url.to_owned(), headers);
    for _ in 0..=MAX_REDIRECTS {
        crate::failure::check(ports.cancel)?;
        validate_url(&current)?;
        let request = Get {
            url: &current,
            headers: &current_headers,
            max_bytes: DOWNLOAD_MAX_BYTES,
            timeout: DOWNLOAD_TIMEOUT,
        };
        let response = ports.http.get(request, ports.cancel).await?;
        if is_redirect(response.status) {
            let Some(location) = response.location else {
                bail!("Plugin zip download redirect is missing Location header: {current}");
            };
            let next = resolve_location(&location, &current)?;
            if !same_origin(&next, &current) {
                current_headers.clear();
            }
            current = next;
            continue;
        }
        if !(200..300).contains(&response.status) {
            return Err(DownloadFailure {
                status: response.status,
                message: format!(
                    "Failed to download plugin zip: {} {}",
                    response.status, response.status_text
                ),
            }
            .into());
        }
        return Ok(response.body);
    }
    bail!("Plugin zip download exceeded redirect limit: {url}")
}

/// Node `resolveZipRoot`.
async fn root(extract: &Path, input: &HttpZip<'_>, top_level: &[String]) -> Result<PathBuf> {
    if input.require_single_root && top_level.len() != 1 {
        bail!(
            "Plugin zip must contain exactly one top-level directory: {}",
            top_level.len()
        );
    }
    if let Some(path) = input.path {
        let requested = within(extract, &normalize(path)?)?;
        if !crate::fsx::is_dir(&requested).await {
            bail!("Plugin zip source subdirectory does not exist: {path}");
        }
        return Ok(requested);
    }
    if crate::manifest::find(extract).await.is_some() {
        return Ok(extract.to_owned());
    }
    if input.strip_root != Some(false)
        && let [segment] = top_level
    {
        let candidate = within(extract, segment)?;
        if crate::fsx::is_dir(&candidate).await {
            return Ok(candidate);
        }
    }
    if !crate::fsx::is_dir(extract).await {
        bail!("Plugin zip did not extract a plugin root directory");
    }
    Ok(extract.to_owned())
}

/// Node `resolveHttpZipSource`.
pub async fn resolve_http(ports: &Ports<'_>, input: HttpZip<'_>) -> Result<Resolved> {
    validate_url(input.url)?;
    validate_headers(&input.headers)?;
    if let Some(path) = input.path {
        normalize(path)?;
    }
    if input
        .sha256
        .is_some_and(|sha| !sha_pattern(&sha.to_lowercase()))
    {
        bail!("Plugin zip source sha256 must be a 64 character hex string");
    }
    let temp = crate::store::temp_dir(TEMP_PREFIX).await?;
    let result = async {
        crate::failure::check(ports.cancel)?;
        let bytes = download(ports, input.url, input.headers.clone()).await?;
        let actual = format!("{:x}", sha2::Sha256::digest(&bytes));
        if let Some(expected) = input.sha256.map(str::to_lowercase)
            && actual != expected
        {
            bail!("Plugin zip sha256 mismatch: expected={expected}, actual={actual}");
        }
        let archive = temp.join("source.zip");
        tokio::fs::write(&archive, bytes).await?;
        let extract = temp.join("extract");
        let (target, cancel) = (extract.clone(), ports.cancel.clone());
        let top_level = tokio::task::spawn_blocking(move || {
            crate::zip_extract::extract(&archive, &target, &cancel)
        })
        .await??;
        root(&extract, &input, &top_level).await
    }
    .await;
    match result {
        Ok(path) => Ok(Resolved {
            path,
            cleanup: Some(temp),
        }),
        Err(error) => {
            let cleanup = crate::store::cleanup(Some(&temp)).await;
            Err(crate::failure::append_cleanup(error, cleanup))
        }
    }
}

/// Node `isZipPluginUrlSource`.
pub fn is_zip(source: Option<&Value>) -> bool {
    source.is_some_and(|s| {
        s["source"] == "url"
            && s["type"] == "zip"
            && s["url"].is_string()
            && s["sha256"].is_string()
    })
}

/// Node `resolvePluginSourceRoot`'s zip branch: the entry fields are read in
/// Node's order, then `resolveZipPluginSource` validates and downloads.
pub async fn resolve_plugin(
    ports: &Ports<'_>,
    source: &Map<String, Value>,
    url: &str,
) -> Result<Resolved> {
    let headers = match source.get("headers") {
        None => vec![],
        Some(Value::Object(headers)) => {
            let mut pairs = vec![];
            for (key, value) in headers {
                let Some(value) = value.as_str() else {
                    bail!("Plugin zip source header must be a string: {key}");
                };
                pairs.push((key.clone(), value.to_owned()));
            }
            pairs
        }
        Some(_) => bail!("Plugin zip source headers must be an object"),
    };
    let path = match source.get("path") {
        None => None,
        Some(Value::String(path)) => Some(path.as_str()),
        Some(_) => bail!("Plugin zip source path must be a string"),
    };
    let Some(sha256) = source.get("sha256").and_then(Value::as_str) else {
        bail!("Plugin zip source sha256 is required");
    };
    let strip_root = match source.get("stripRoot") {
        None => None,
        Some(Value::Bool(strip)) => Some(*strip),
        Some(_) => bail!("Plugin zip source stripRoot must be a boolean"),
    };
    validate_url(url)?;
    if !sha_pattern(&sha256.to_lowercase()) {
        bail!("Plugin zip source sha256 must be a 64 character hex string");
    }
    let input = HttpZip {
        url,
        headers,
        path,
        sha256: Some(sha256),
        strip_root,
        require_single_root: false,
    };
    resolve_http(ports, input).await
}
