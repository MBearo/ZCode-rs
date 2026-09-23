use anyhow::{Context, Result, bail};
use clap::Parser;
use serde::Deserialize;
use std::path::PathBuf;

pub fn question_timing() -> Result<(u64, u64)> {
    let mut scale = 1.0;
    if std::env::var("ZCODE_ENV").as_deref() == Ok("test")
        && let Ok(raw) = std::env::var("ZCODE_E2E_ASK_USER_QUESTION_CLOCK_SCALE")
        && !raw.trim().is_empty()
    {
        scale = raw
            .trim()
            .parse::<f64>()
            .context("Invalid AskUserQuestion clock scale")?;
        anyhow::ensure!(
            scale.is_finite() && (1.0..=1000.0).contains(&scale),
            "AskUserQuestion clock scale must be between 1 and 1000"
        );
    }
    Ok((
        (60_000.0 / scale).round().max(1.0) as u64,
        (300_000.0 / scale).round().max(1.0) as u64,
    ))
}

#[derive(Parser, Debug)]
#[command(version, about = "Headless ZCode Rust core (App stdio)")]
pub struct Args {
    #[arg(value_parser=["app-server"])]
    pub command: String,
    #[arg(long, required = true)]
    pub stdio: bool,
    #[arg(long)]
    pub cwd: Option<PathBuf>,
    #[arg(long)]
    pub data_dir: Option<PathBuf>,
    #[arg(long)]
    pub import_ts_db: Option<PathBuf>,
    #[arg(long)]
    pub config: Option<PathBuf>,
    #[arg(long, value_parser=["desktop","terminal"], default_value="terminal")]
    pub surface: String,
    #[arg(long)]
    pub prepare_storage: bool,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelConfig {
    #[serde(default)]
    pub format_properties: Option<serde_json::Value>,
    #[serde(skip)]
    pub max_output_map: Option<String>,
    #[serde(skip)]
    pub api_key_value: Option<String>,
    #[serde(skip)]
    pub headers: std::collections::BTreeMap<String, String>,
    #[serde(skip)]
    pub account_access: Option<serde_json::Value>,
    #[serde(skip)]
    pub option_patches: Vec<serde_json::Value>,
    #[serde(default)]
    pub api_type: super::model_protocol::ApiType,
    #[serde(default = "default_context_window")]
    pub context_window: usize,
    #[serde(default = "default_max_output")]
    pub max_output_tokens: usize,
    #[serde(default = "default_context_buffer")]
    pub context_buffer_tokens: usize,
    #[serde(default = "default_auto_compact")]
    pub auto_compact: bool,
    pub provider_id: String,
    pub model_id: String,
    pub reasoning_level: String,
    #[serde(default)]
    pub reasoning_parameters: serde_json::Map<String, serde_json::Value>,
    pub base_url: String,
    pub api_key_env: Option<String>,
    pub request_timeout_seconds: Option<u64>,
    #[serde(default = "default_idle")]
    pub stream_idle_timeout_ms: u64,
    #[serde(default)]
    pub retry: super::model_policy::RetryConfig,
}
fn default_context_window() -> usize {
    200_000
}
fn default_max_output() -> usize {
    32_000
}
fn default_context_buffer() -> usize {
    13_000
}
fn default_auto_compact() -> bool {
    true
}
fn default_idle() -> u64 {
    600_000
}

impl ModelConfig {
    pub async fn load(path: Option<&PathBuf>) -> Result<Option<Self>> {
        let Some(path) = path else { return Ok(None) };
        let data = tokio::fs::read(path)
            .await
            .context("Cannot read model config")?;
        let config: Self = serde_json::from_slice(&data).context("Invalid model config")?;
        if config.max_output_tokens == 0
            || config.max_output_tokens >= config.context_window
            || config
                .context_window
                .saturating_sub(config.max_output_tokens.min(21_000))
                .saturating_sub(config.context_buffer_tokens)
                == 0
        {
            bail!("Invalid context/output budget");
        }
        let url = reqwest::Url::parse(&config.base_url).context("Invalid provider URL")?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            bail!("Provider URL must be http(s), without credentials, query or fragment");
        }
        if config.provider_id.trim().is_empty()
            || config.model_id.trim().is_empty()
            || config.reasoning_level.trim().is_empty()
            || config.request_timeout_seconds == Some(0)
            || config
                .retry
                .backoff_factor
                .is_some_and(|n| !n.is_finite() || n <= 0.0)
        {
            bail!("Invalid provider/model/timeout");
        }
        if config
            .reasoning_parameters
            .keys()
            .any(|key| !config.api_type.reasoning_key(key))
        {
            bail!("Unsupported reasoning parameter for selected API protocol");
        }
        if config.api_type == super::model_protocol::ApiType::Responses
            && config.reasoning_parameters.contains_key("reasoning")
            && config.reasoning_parameters.contains_key("reasoning_effort")
        {
            bail!("Responses reasoning and reasoning_effort are mutually exclusive");
        }
        Ok(Some(config))
    }
    pub fn api_key(&self) -> Result<Option<String>> {
        if let Some(key) = &self.api_key_value {
            return Ok(Some(key.clone()));
        }
        match &self.api_key_env {
            Some(name) => Ok(Some(
                std::env::var(name)
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .context("Provider API key environment variable is missing")?,
            )),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn long_streams_have_no_implicit_total_deadline() {
        let config: ModelConfig = serde_json::from_value(serde_json::json!({
            "providerId":"fixture","modelId":"fixture","reasoningLevel":"none","baseUrl":"https://example.invalid"
        })).unwrap();
        assert_eq!(config.request_timeout_seconds, None);
        assert_eq!(config.stream_idle_timeout_ms, 600_000);
    }
}
