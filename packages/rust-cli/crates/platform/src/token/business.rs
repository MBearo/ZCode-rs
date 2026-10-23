// 语义复制自 apps/zcode-cli/packages/adapters/src/auth/zai-business-token-cache.ts（33a9541140）。
//! Z.ai 业务 token（platform.md §5.4、§5.6）：Z.ai 的项目接口不认 OAuth access token，先用它换业务 JWT。
//! 修复线 F47 起只在登录时换一次，存进 `oauth:zai:access_token`（App 的语义），请求期不再换。
//!
//! 进程内只保留一个会话（按 access token）。有效期取 JWT `exp` 与 `expires_in` 的较早者，都没有时
//! 300 秒；刷新点提前 120 秒。临时故障时，已知有效期且没过期的旧 JWT 继续用，5 秒后再试。

use base64::Engine;
use rust_cli_base::secret::RedactedString;
use serde_json::{Value, json};

use super::http::{Auth, Fault, Reply, success_code};
use super::{ProjectTokens, REFRESH_LEAD_MS, RETRY_BACKOFF_MS, TokenError, lock};

const UNKNOWN_TTL_MS: u64 = 300_000;

#[derive(Debug, Clone)]
pub(crate) struct BusinessToken {
    pub token: RedactedString,
    refresh_at: u64,
    expires_at: Option<u64>,
}

/// 一个 access token 的业务 token 状态。
pub(crate) struct BusinessSession {
    oauth: String,
    value: Option<BusinessToken>,
    retry_at: Option<u64>,
}

impl BusinessSession {
    /// 到刷新点（或临时故障后的重试时刻）之前、且没过期的业务 JWT。
    pub(super) fn usable(&self, now: u64) -> Option<BusinessToken> {
        let value = self.value.as_ref()?;
        let until = self
            .retry_at
            .unwrap_or(value.refresh_at)
            .min(value.expires_at.unwrap_or(u64::MAX));
        (now < until).then(|| value.clone())
    }
}

impl ProjectTokens {
    /// 当前可用的业务 token；没有或到了刷新点时换一个。失败一律是 `project_token_login_failed`。
    pub(crate) async fn business(&self, oauth: &str) -> Result<BusinessToken, TokenError> {
        {
            let mut state = lock(&self.state);
            let fresh = state
                .business
                .as_ref()
                .is_none_or(|session| session.oauth != oauth);
            if fresh {
                state.business = Some(BusinessSession {
                    oauth: oauth.to_owned(),
                    value: None,
                    retry_at: None,
                });
            }
            if let Some(value) = state
                .business
                .as_ref()
                .and_then(|session| session.usable(self.clock.now_ms()))
            {
                return Ok(value);
            }
        }
        let started = self.clock.now_ms();
        let reply = self
            .http
            .post(
                format!("{}/api/auth/z/login", self.origins.zai_api),
                Auth::None,
                &json!({"token": oauth}),
            )
            .await;
        let now = self.clock.now_ms();
        let mut state = lock(&self.state);
        // 换取期间凭据被清空或换了账号：结果作废。
        let Some(session) = state
            .business
            .as_mut()
            .filter(|session| session.oauth == oauth)
        else {
            return Err(TokenError::LoginFailed);
        };
        match reply.map(|reply| parse(&reply, started, now)) {
            Ok(Some(value)) => {
                session.value = Some(value.clone());
                session.retry_at = None;
                Ok(value)
            }
            Err(Fault::Transient) => {
                // 真实有效期未知时不能降级。
                let kept = session
                    .value
                    .clone()
                    .filter(|value| value.expires_at.is_some_and(|expires| now < expires));
                match kept {
                    Some(value) => {
                        let expires = value.expires_at.unwrap_or(now);
                        session.retry_at = Some((now + RETRY_BACKOFF_MS).min(expires));
                        tracing::warn!(code = "project_token_business_login_refresh_deferred");
                        Ok(value)
                    }
                    None => {
                        session.value = None;
                        session.retry_at = None;
                        Err(TokenError::LoginFailed)
                    }
                }
            }
            Ok(None) | Err(Fault::Unauthorized | Fault::Forbidden | Fault::Failed) => {
                session.value = None;
                session.retry_at = None;
                Err(TokenError::LoginFailed)
            }
        }
    }
}

/// 换取响应：`code` 缺省或是成功码；`data.access_token` / `accessToken`。
fn parse(reply: &Reply, started: u64, now: u64) -> Option<BusinessToken> {
    let Reply::Json(value) = reply else {
        return None;
    };
    if value
        .get("code")
        .is_some_and(|code| !code.is_null() && !success_code(code))
    {
        return None;
    }
    let data = &value["data"];
    let token = [&data["access_token"], &data["accessToken"]]
        .into_iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .find(|token| !token.is_empty())?
        .to_owned();
    let ttl = [&data["expires_in"], &data["expiresIn"]]
        .into_iter()
        .find(|ttl| !ttl.is_null())
        .and_then(Value::as_f64)
        .filter(|ttl| ttl.is_finite());
    let deadlines = [
        jwt_expiry(&token),
        ttl.map(|ttl| (started as f64 + ttl * 1000.0) as u64),
    ];
    let expires_at = deadlines.into_iter().flatten().min();
    let cache_until = expires_at.unwrap_or(started + UNKNOWN_TTL_MS);
    if cache_until <= now {
        return None;
    }
    Some(BusinessToken {
        token: RedactedString::new(token),
        refresh_at: cache_until.saturating_sub(REFRESH_LEAD_MS),
        expires_at,
    })
}

/// JWT 载荷里的 `exp`（秒）→ 毫秒；读不出时为空。
fn jwt_expiry(token: &str) -> Option<u64> {
    let payload = token.split('.').nth(1)?;
    let engine = base64::engine::GeneralPurpose::new(
        &base64::alphabet::URL_SAFE,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    );
    let bytes = engine.decode(payload).ok()?;
    let exp = serde_json::from_slice::<Value>(&bytes).ok()?["exp"].as_f64()?;
    // 负数（早已过期）落到 0：随后按“已过期”拒绝，与 Node 相同。
    exp.is_finite().then(|| (exp * 1000.0).max(0.0) as u64)
}

#[cfg(test)]
#[path = "business_tests.rs"]
mod tests;
