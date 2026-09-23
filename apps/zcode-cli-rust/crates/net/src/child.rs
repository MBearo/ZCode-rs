//! Tool child-process environment (Node `adapters/src/network/subprocess-env.ts`).
use crate::{
    env::{PASSTHROUGH_KEY, read_passthrough, same_key, should_sanitize},
    proxy::has_scheme,
};
use serde_json::Value;

/// The `network` section of the startup configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetworkPolicy {
    pub http_proxy: Option<String>,
    pub no_proxy: Option<String>,
    pub ca_cert_file: Option<String>,
}

impl NetworkPolicy {
    pub fn from_config(network: &Value) -> Self {
        let text = |key: &str| network.get(key).and_then(Value::as_str).map(str::to_owned);
        Self {
            http_proxy: text("httpProxy"),
            no_proxy: text("noProxy"),
            ca_cert_file: text("caCertFile"),
        }
    }
}

const PROXY_KEYS: [&str; 6] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
];
const NO_PROXY_KEYS: [&str; 2] = ["NO_PROXY", "no_proxy"];
const CA_KEYS: [&str; 5] = [
    "NODE_EXTRA_CA_CERTS",
    "SSL_CERT_FILE",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
    "GIT_SSL_CAINFO",
];

fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

/// Node `getFirstEnvValue`: the untrimmed value when it is not blank.
fn raw<'a>(source: &'a [(String, String)], key: &str, windows: bool) -> Option<&'a str> {
    source
        .iter()
        .find(|(k, _)| same_key(k, key, windows))
        .map(|(_, v)| v.as_str())
        .filter(|v| !v.trim().is_empty())
}

/// Node `setEnvKey`: on Windows every case variant is replaced.
fn set(env: &mut Vec<(String, String)>, key: &str, value: &str, windows: bool) {
    env.retain(|(k, _)| !same_key(k, key, windows));
    env.push((key.into(), value.into()));
}

/// Node `normalizeProxyValue` (no URL parsing, unlike request proxies).
fn proxy_value(value: Option<&str>) -> Option<String> {
    let value = trimmed(value)?;
    Some(if has_scheme(value) {
        value.into()
    } else {
        format!("http://{value}")
    })
}

/// Node `applyNetworkEgressEnv(sanitizeZCodeRuntimeEnv(source), { network, sourceEnv: source })`.
pub fn tool_env(
    source: &[(String, String)],
    network: &NetworkPolicy,
    windows: bool,
) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = source
        .iter()
        .filter(|(key, _)| !should_sanitize(key))
        .cloned()
        .collect();
    env.retain(|(key, _)| !same_key(key, PASSTHROUGH_KEY, windows));
    let passthrough = source
        .iter()
        .find(|(k, _)| same_key(k, PASSTHROUGH_KEY, windows))
        .map(|(_, v)| v.as_str());
    for (key, value) in read_passthrough(passthrough) {
        set(&mut env, &key, &value, windows);
    }
    let proxy = proxy_value(network.http_proxy.as_deref())
        .or_else(|| proxy_value(raw(source, "ZCODE_HTTP_PROXY", windows)));
    if let Some(proxy) = proxy {
        for key in PROXY_KEYS {
            set(&mut env, key, &proxy, windows);
        }
    }
    let no_proxy = trimmed(network.no_proxy.as_deref())
        .or_else(|| raw(source, "ZCODE_NO_PROXY", windows))
        .map(str::to_owned);
    if let Some(no_proxy) = no_proxy {
        for key in NO_PROXY_KEYS {
            set(&mut env, key, &no_proxy, windows);
        }
    }
    let ca = trimmed(network.ca_cert_file.as_deref())
        .or_else(|| raw(source, "ZCODE_AGENT_CA_CERT", windows))
        .map(str::to_owned);
    if let Some(ca) = ca {
        for key in CA_KEYS {
            set(&mut env, key, &ca, windows);
        }
    }
    env
}
