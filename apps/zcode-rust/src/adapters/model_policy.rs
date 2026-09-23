use serde::Deserialize;

#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RetryConfig {
    pub max_retries: Option<u32>,
    pub base_delay_ms: Option<u64>,
    pub backoff_factor: Option<f64>,
    pub max_delay_ms: Option<u64>,
    pub jitter: Option<bool>,
}
pub struct RetryPolicy {
    pub max_attempts: u32,
    base: u64,
    factor: f64,
    max_delay: u64,
    jitter: bool,
}
impl RetryPolicy {
    pub fn resolve(config: &RetryConfig) -> Self {
        let env = |name: &str| {
            std::env::var(name)
                .ok()
                .filter(|s| !s.trim().is_empty())
                .and_then(|s| s.trim().parse::<f64>().ok())
                .filter(|n| n.is_finite() && *n >= 0.0)
        };
        Self {
            max_attempts: config
                .max_retries
                .map(u64::from)
                .or_else(|| env("ZCODE_MODEL_RETRY_MAX_RETRIES").map(|n| n as u64))
                .unwrap_or(10)
                .min(u32::MAX as u64 - 1) as u32
                + 1,
            base: config
                .base_delay_ms
                .or_else(|| env("ZCODE_MODEL_RETRY_BASE_DELAY_MS").map(|n| n as u64))
                .unwrap_or(2000),
            factor: config
                .backoff_factor
                .or_else(|| env("ZCODE_MODEL_RETRY_BACKOFF_FACTOR").filter(|n| *n > 0.0))
                .unwrap_or(2.0),
            max_delay: config
                .max_delay_ms
                .or_else(|| env("ZCODE_MODEL_RETRY_MAX_DELAY_MS").map(|n| n as u64))
                .unwrap_or(60000),
            jitter: config.jitter.unwrap_or(true),
        }
    }
    pub fn delay_ms(&self, attempt: u32, retry_after: Option<u64>, random: f64) -> u64 {
        let uncapped = self.base as f64 * self.factor.powf(attempt.saturating_sub(1) as f64);
        // 与 TS runner-retry 一致：合理的供应商等待不受本地 60 秒上限截短。
        if let Some(ms) = retry_after
            && (ms <= 300_000 || (ms as f64) < uncapped)
        {
            return ms;
        }
        let capped = uncapped.min(self.max_delay as f64);
        (capped
            * if self.jitter {
                0.5 + random.clamp(0.0, 1.0) * 0.5
            } else {
                1.0
            })
        .round() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retry_delay_matches_ts_and_preserves_provider_wait() {
        let policy = RetryPolicy {
            max_attempts: 11,
            base: 2000,
            factor: 2.0,
            max_delay: 60000,
            jitter: true,
        };
        assert_eq!(policy.delay_ms(1, None, 0.0), 1000);
        assert_eq!(policy.delay_ms(2, None, 1.0), 4000);
        assert_eq!(policy.delay_ms(10, None, 0.5), 45000);
        assert_eq!(policy.delay_ms(1, Some(120000), 0.0), 120000);
        assert_eq!(policy.delay_ms(1, Some(900000), 0.0), 1000);
        assert_eq!(policy.delay_ms(10, Some(900000), 0.0), 900000);
    }
}
