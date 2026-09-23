//! Process environment view captured once at startup. Equivalent to Node's
//! `process.env` after `applyCliRuntimeEnvSanitization` (`cli/src/env.ts`,
//! `shared/src/runtimeEnv.ts`); the real process environment is never mutated.
use serde_json::Value;
use std::{cmp::Ordering, collections::BTreeMap, path::Path};

pub const PASSTHROUGH_KEY: &str = "ZCODE_TOOL_ENV_PASSTHROUGH_JSON";

/// Node `SANITIZED_RUNTIME_ENV_KEYS`.
const SANITIZED: &[&str] = &[
    "NODE_ENV",
    "ELECTRON_RUN_AS_NODE",
    "NODE_NO_WARNINGS",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "NODE_EXTRA_CA_CERTS",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
    "GIT_SSL_CAINFO",
    "ZCODE_REMOTE_RUNTIME_NETWORK_AUTHORITY",
    "ZCODE_REMOTE_HTTP_PROXY",
    "ZCODE_REMOTE_NO_PROXY",
    "ZCODE_CUA_PERMISSION_BROKER_SOCKET",
    "ZCODE_CUA_PERMISSION_BROKER_TOKEN",
    "ZCODE_CUA_PERMISSION_BROKER_REFRESH_MARKER",
    "ZCODE_CUA_PLUGIN_AUTHORITY",
    "OTEL_EXPORTER_OTLP_ENDPOINT",
    "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
    "OTEL_EXPORTER_OTLP_HEADERS",
    "OTEL_EXPORTER_OTLP_TRACES_HEADERS",
    "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
    "OTEL_EXPORTER_OTLP_METRICS_HEADERS",
    "OTEL_SERVICE_NAME",
    "OTEL_RESOURCE_ATTRIBUTES",
    "OTEL_EXPORTER_OTLP_COMPRESSION",
    "ZCODE_MODEL_TELEMETRY_ENABLED",
    "ZCODE_TELEMETRY_DEVICE_MID",
    "ZCODE_TELEMETRY_USER_ID",
    "ZCODE_TELEMETRY_USER_ID_HASH",
    "ZCODE_TELEMETRY_USER_SUBJECT_ID",
    "ZCODE_TELEMETRY_IDENTITY_STATE",
    "ZCODE_TELEMETRY_RUNTIME_SURFACE",
    "ZCODE_TELEMETRY_RUNTIME_DISTRIBUTION",
];

/// Node `NON_TOOL_PASSTHROUGH_RUNTIME_ENV_KEYS`.
const NON_TOOL_PASSTHROUGH: &[&str] = &[
    "NODE_ENV",
    "ELECTRON_RUN_AS_NODE",
    "NODE_NO_WARNINGS",
    "ZCODE_CUA_PERMISSION_BROKER_SOCKET",
    "ZCODE_CUA_PERMISSION_BROKER_REFRESH_MARKER",
    "ZCODE_CUA_PLUGIN_AUTHORITY",
    "ZCODE_REMOTE_RUNTIME_NETWORK_AUTHORITY",
    "ZCODE_REMOTE_HTTP_PROXY",
    "ZCODE_REMOTE_NO_PROXY",
];

/// Node `SANITIZED_PACKAGE_MANAGER_ENV_PATTERN` (case-insensitive).
fn package_manager_network_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    ["npm_config_", "yarn_", "pnpm_"].iter().any(|prefix| {
        lower.strip_prefix(prefix).is_some_and(|rest| {
            matches!(
                rest,
                "http_proxy" | "https_proxy" | "proxy" | "all_proxy" | "no_proxy" | "cafile" | "ca"
            )
        })
    })
}

fn telemetry_key(upper: &str) -> bool {
    upper.starts_with("OTEL_")
        || upper.starts_with("ZCODE_TELEMETRY_")
        || upper == "ZCODE_MODEL_TELEMETRY_ENABLED"
}

/// Node `shouldSanitizeZCodeRuntimeEnvKey`.
pub fn should_sanitize(key: &str) -> bool {
    SANITIZED.contains(&key.to_uppercase().as_str()) || package_manager_network_key(key)
}

/// Node `shouldCaptureZCodeToolEnvPassthroughKey`.
pub fn should_capture(key: &str) -> bool {
    let upper = key.to_uppercase();
    !telemetry_key(&upper)
        && !NON_TOOL_PASSTHROUGH.contains(&upper.as_str())
        && should_sanitize(key)
}

fn identifier(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Node `readZCodeToolEnvPassthroughEnv`: invalid JSON or entries are ignored.
pub fn read_passthrough(raw: Option<&str>) -> BTreeMap<String, String> {
    let Some(Ok(Value::Object(parsed))) = raw
        .filter(|r| !r.is_empty())
        .map(serde_json::from_str::<Value>)
    else {
        return BTreeMap::new();
    };
    parsed
        .into_iter()
        .filter_map(|(key, value)| match value {
            Value::String(value) if identifier(&key) && should_capture(&key) => Some((key, value)),
            _ => None,
        })
        .collect()
}

/// `String.prototype.localeCompare` (ICU root collation) for environment keys:
/// punctuation < digits < letters, letters case-insensitive with lowercase first
/// as the tie-breaker.
fn locale_cmp(left: &str, right: &str) -> Ordering {
    fn primary(c: char) -> (u8, u32) {
        if c.is_ascii_digit() {
            (1, c as u32)
        } else if c.is_alphabetic() {
            (2, c.to_lowercase().next().unwrap_or(c) as u32)
        } else {
            (0, c as u32)
        }
    }
    let primary_order = left.chars().map(primary).cmp(right.chars().map(primary));
    primary_order.then_with(|| {
        let tertiary = |c: char| u8::from(c.is_uppercase());
        left.chars().map(tertiary).cmp(right.chars().map(tertiary))
    })
}

/// Node `stringifyZCodeToolEnvPassthroughEnv`.
fn stringify_passthrough(captured: BTreeMap<String, String>) -> Option<String> {
    if captured.is_empty() {
        return None;
    }
    let mut entries: Vec<_> = captured.into_iter().collect();
    entries.sort_by(|(left, _), (right, _)| locale_cmp(left, right));
    let body: Vec<String> = entries
        .iter()
        .map(|(key, value)| {
            format!(
                "{}:{}",
                Value::from(key.as_str()),
                Value::from(value.as_str())
            )
        })
        .collect();
    Some(format!("{{{}}}", body.join(",")))
}

/// Case-insensitive on Windows like Node's `process.env`.
pub(crate) fn same_key(left: &str, right: &str, windows: bool) -> bool {
    if windows {
        left.to_lowercase() == right.to_lowercase()
    } else {
        left == right
    }
}

/// Node `/(^|[/\\])zcode-beta(?:$|\.)/u`.
fn invoked_as_beta(arg: &str) -> bool {
    const NAME: &str = "zcode-beta";
    arg.match_indices(NAME).any(|(at, _)| {
        let before = arg[..at].chars().next_back();
        let after = arg[at + NAME.len()..].chars().next();
        matches!(before, None | Some('/' | '\\')) && matches!(after, None | Some('.'))
    })
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuntimeEnv {
    vars: Vec<(String, String)>,
}

impl RuntimeEnv {
    /// Node `prepareCliRuntimeEnv` for a compiled (non-`.ts`) entry point.
    pub fn capture(
        env: impl IntoIterator<Item = (String, String)>,
        home: &Path,
        argv: &[String],
    ) -> Self {
        let source: Vec<(String, String)> = env.into_iter().collect();
        let windows = cfg!(windows);
        let lookup = |key: &str| {
            source
                .iter()
                .find(|(k, _)| same_key(k, key, windows))
                .map(|(_, v)| v.as_str())
        };
        let mut captured = read_passthrough(lookup(PASSTHROUGH_KEY));
        for (key, value) in &source {
            if should_capture(key) {
                captured.insert(key.clone(), value.clone());
            }
        }
        let mut result = Self {
            vars: source
                .iter()
                .filter(|(key, _)| !should_sanitize(key))
                .cloned()
                .collect(),
        };
        if let Some(json) = stringify_passthrough(captured) {
            result.set(PASSTHROUGH_KEY, json);
        }
        let runtime = result
            .get("ZCODE_RUNTIME_ENV")
            .map(|v| v.trim().to_lowercase())
            .filter(|v| matches!(v.as_str(), "development" | "production" | "test"))
            .unwrap_or_else(|| "production".into());
        result.set("ZCODE_RUNTIME_ENV", runtime);
        // Node `applyBetaStorageDefault`：未显式指定存储目录的 beta 入口使用独立目录。
        let beta = result.get("ZCODE_BETA") == Some("1")
            || result.get("ZCODE_ENV") == Some("beta")
            || argv.iter().any(|arg| invoked_as_beta(arg));
        if beta
            && result
                .get("ZCODE_STORAGE_DIR")
                .is_none_or(|v| v.trim().is_empty())
        {
            result.set(
                "ZCODE_STORAGE_DIR",
                home.join(".zcode-beta").to_string_lossy().into_owned(),
            );
        }
        result
    }

    /// A view used as-is (tests and embedders that already sanitized the environment).
    pub fn from_vars(vars: Vec<(String, String)>) -> Self {
        Self { vars }
    }

    pub fn vars(&self) -> &[(String, String)] {
        &self.vars
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.vars
            .iter()
            .find(|(k, _)| same_key(k, key, cfg!(windows)))
            .map(|(_, v)| v.as_str())
    }

    fn set(&mut self, key: &str, value: String) {
        match self
            .vars
            .iter_mut()
            .find(|(k, _)| same_key(k, key, cfg!(windows)))
        {
            Some(entry) => entry.1 = value,
            None => self.vars.push((key.into(), value)),
        }
    }

    /// Node `readZCodeToolEnvPassthroughEnv(process.env)`.
    pub fn passthrough(&self) -> BTreeMap<String, String> {
        read_passthrough(self.get(PASSTHROUGH_KEY))
    }
}
