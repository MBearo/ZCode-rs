// 语义复制自 packages/shared/src/project-access-token-client.ts 的 `resolve` 与
// packages/shared/src/project-access-token.ts 的 `ProjectAccessTokenCache`（33a9541140）。
//! 一个 login token 下的换取（platform.md §5.4）：会话 → 项目位置 → 签发 PAT。
//!
//! 错误按 Node 归一：`project_token_invalid_response`、`project_token_scope_invalidated`、
//! `project_token_disabled`、`project_token_project_unavailable` 原样给出，其余都是
//! `project_token_request_failed`。

use ring::rand::{SecureRandom, SystemRandom};
use rust_cli_base::secret::RedactedString;
use rust_cli_base::units;
use serde_json::{Value, json};

use super::http::{Auth, Fault, Reply, success_code};
use super::location::{self, Location, TeamProject};
use super::{
    Account, Family, Issued, Material, ProjectTokens, REFRESH_LEAD_MS, RETRY_BACKOFF_MS, Session,
    State, TokenError, fingerprint, lock,
};
use crate::credentials::CredentialStore;

const REFRESH_JITTER_MS: f64 = 60_000.0;

pub(super) struct Input<'a> {
    pub family: Family,
    /// zai 是业务 JWT，bigmodel 是 access token（已去掉 `Bearer ` 前缀）。
    pub login: &'a str,
    /// 项目位置键里的账号。
    pub account: &'a str,
    /// 团队项目；个人为空。
    pub team: Option<&'a TeamProject>,
    /// 进程内缓存的范围（[`location::scope_key`]）。
    pub scope: &'a str,
    pub epoch: u64,
}

/// 会话的身份：换取期间会话被替换或清空时，结果不再写回。
struct Scope {
    origin: String,
    family: Family,
    generation: u64,
    epoch: u64,
}

impl Scope {
    fn session<'s>(&self, state: &'s mut State) -> Result<&'s mut Session, TokenError> {
        if state.epoch != self.epoch {
            return Err(TokenError::ScopeInvalidated);
        }
        state
            .sessions
            .get_mut(&(self.origin.clone(), self.family))
            .filter(|session| session.generation == self.generation)
            .ok_or(TokenError::ScopeInvalidated)
    }
}

impl ProjectTokens {
    /// App `ProjectAccessTokenClient.resolve`（个人 non-team，或团队项目）。
    pub(super) async fn resolve_with(
        &self,
        input: Input<'_>,
        store: &CredentialStore,
    ) -> Result<Material, TokenError> {
        if input.login.is_empty() || input.account.trim().is_empty() {
            return Err(TokenError::LoginRequired);
        }
        let session = self.open_session(input.family, input.login, input.epoch);
        let result = async {
            let location = self.location(&session, &input, store).await?;
            self.issue(&session, &input, location, store).await
        }
        .await;
        result.map_err(|error| match error {
            TokenError::InvalidResponse
            | TokenError::ScopeInvalidated
            | TokenError::Disabled
            | TokenError::ProjectUnavailable => error,
            TokenError::LoginRequired | TokenError::LoginFailed | TokenError::RequestFailed => {
                TokenError::RequestFailed
            }
        })
    }

    /// 取 (origin, family) 的会话；login token 变了就换一个新会话（旧的位置与 PAT 一起丢掉）。
    fn open_session(&self, family: Family, login: &str, epoch: u64) -> Scope {
        let origin = self.origin(family).to_owned();
        let mut state = lock(&self.state);
        state.next_generation += 1;
        let generation = state.next_generation;
        let key = (origin.clone(), family);
        let session = state.sessions.entry(key).or_insert_with(|| Session {
            login_token: login.to_owned(),
            generation,
            accounts: Default::default(),
        });
        if session.login_token != login {
            *session = Session {
                login_token: login.to_owned(),
                generation,
                accounts: Default::default(),
            };
        }
        Scope {
            origin,
            family,
            generation: session.generation,
            epoch,
        }
    }

    /// 项目位置：内存 → `credentials.json`（团队要组织与项目对得上）→ 重新解析（写回 `credentials.json`）。
    async fn location(
        &self,
        scope: &Scope,
        input: &Input<'_>,
        store: &CredentialStore,
    ) -> Result<Location, TokenError> {
        {
            let mut state = lock(&self.state);
            let session = scope.session(&mut state)?;
            if let Some(location) = session
                .accounts
                .get(input.scope)
                .and_then(|a| a.location.clone())
            {
                return Ok(location);
            }
        }
        let key = location::storage_key(&scope.origin, scope.family, input.account, input.team);
        // 解不开的位置记录：Node 的 CLI 存放处没有恢复端口，同样报请求失败。
        let saved = store
            .read()
            .await
            .and_then(|credentials| credentials.get(&key))
            .map_err(|_| TokenError::RequestFailed)?;
        let saved = saved
            .and_then(|saved| Location::parse(saved.expose()))
            .filter(|location| location.matches(input.team));
        let location = match saved {
            Some(location) => location,
            None => {
                let lookup = location::Lookup {
                    origin: &scope.origin,
                    family: scope.family,
                    login_token: input.login,
                    team: input.team,
                };
                let location = location::resolve(&self.http, lookup).await?;
                scope.session(&mut lock(&self.state))?;
                store
                    .mutate(|credentials| credentials.set(&key, &location.to_json()))
                    .await
                    .map_err(|_| TokenError::RequestFailed)?;
                location
            }
        };
        let mut state = lock(&self.state);
        let session = scope.session(&mut state)?;
        session
            .accounts
            .entry(input.scope.to_owned())
            .or_default()
            .location = Some(location.clone());
        Ok(location)
    }

    /// PAT：缓存未到刷新点时直接给出，否则签发（Node `ProjectAccessTokenCache.resolve`）。
    async fn issue(
        &self,
        scope: &Scope,
        input: &Input<'_>,
        location: Location,
        store: &CredentialStore,
    ) -> Result<Material, TokenError> {
        let material = |issued: &Issued| Material {
            token: issued.token.clone(),
            location: location.clone(),
        };
        {
            let mut state = lock(&self.state);
            let now = self.clock.now_ms();
            let session = scope.session(&mut state)?;
            if let Some(issued) = session
                .accounts
                .get(input.scope)
                .and_then(|a| a.issued.as_ref())
                && now < issued.refresh_at.min(issued.expires_at)
            {
                return Ok(material(issued));
            }
        }
        let started = self.clock.now_ms();
        let body = json!({
            "clientType": rust_cli_base::names::CLIENT_NAME,
            "clientVersion": self.origins.client_version
        });
        let reply = self
            .http
            .post(
                location.issue_url(&scope.origin),
                Auth::Bearer(input.login),
                &body,
            )
            .await;
        let now = self.clock.now_ms();
        if matches!(reply, Ok(Reply::NotFound)) {
            // 只有明确不存在才废弃位置；权限、网络错误不触发重建。
            self.forget_location(scope, input, store).await;
        }
        let mut state = lock(&self.state);
        let session = scope.session(&mut state)?;
        let entry: &mut Account = session.accounts.entry(input.scope.to_owned()).or_default();
        let outcome = match reply {
            Ok(Reply::Json(value)) => parse(&value, started, now),
            Ok(Reply::NotFound) => Err(TokenError::InvalidResponse),
            Err(Fault::Transient) => {
                // 临时故障：仍在有效期内（且没被 401 作废）的 PAT 继续用，5 秒后再试。
                if let Some(issued) = entry.issued.as_mut().filter(|i| now < i.expires_at) {
                    issued.refresh_at = (now + RETRY_BACKOFF_MS).min(issued.expires_at);
                    tracing::warn!(code = "project_token_refresh_deferred");
                    return Ok(material(issued));
                }
                Err(TokenError::RequestFailed)
            }
            Err(Fault::Unauthorized | Fault::Forbidden | Fault::Failed) => {
                Err(TokenError::RequestFailed)
            }
        };
        match outcome {
            Ok(issued) => {
                let result = material(&issued);
                entry.issued = Some(issued);
                Ok(result)
            }
            Err(error) => {
                entry.issued = None;
                Err(error)
            }
        }
    }

    async fn forget_location(&self, scope: &Scope, input: &Input<'_>, store: &CredentialStore) {
        {
            let mut state = lock(&self.state);
            if let Ok(session) = scope.session(&mut state)
                && let Some(entry) = session.accounts.get_mut(input.scope)
            {
                entry.location = None;
            }
        }
        let key = location::storage_key(&scope.origin, scope.family, input.account, input.team);
        let removed = store
            .mutate(|credentials| {
                credentials.remove(&key);
                Ok(())
            })
            .await;
        if let Err(error) = removed {
            tracing::warn!(%error, "cannot forget the project key location");
        }
    }
}

/// 签发响应（Node `responseSchema`）。`data` 没有 `enable` 时按 `true`。
fn parse(value: &Value, started: u64, now: u64) -> Result<Issued, TokenError> {
    let invalid = TokenError::InvalidResponse;
    if !value.is_object() || !success_code(&value["code"]) || !value["data"].is_object() {
        return Err(invalid);
    }
    let data = &value["data"];
    match data.get("enable") {
        None | Some(Value::Bool(true)) => {}
        Some(Value::Bool(false)) => return Err(TokenError::Disabled),
        Some(_) => return Err(invalid),
    }
    let token = data["accessToken"]
        .as_str()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or(invalid)?;
    let positive = |name: &str| {
        data[name]
            .as_f64()
            .filter(|number| number.is_finite() && *number > 0.0)
            .ok_or(invalid)
    };
    if data["tokenType"] != "Bearer" {
        return Err(invalid);
    }
    let (expires_in, expires_at) = (positive("expiresIn")?, positive("expiresAt")?);
    // expiresIn 从发出签发请求时保守起算，网络耗时不变成额外有效期。
    let expires_at = (expires_at * units::MS_PER_SECOND_F64)
        .min(started as f64 + expires_in * units::MS_PER_SECOND_F64);
    if !expires_at.is_finite() || expires_at <= now as f64 {
        return Err(invalid);
    }
    let expires_at = expires_at as u64;
    let jitter = (random_unit() * REFRESH_JITTER_MS) as u64;
    Ok(Issued {
        token: RedactedString::new(token),
        fingerprint: fingerprint(token),
        expires_at,
        refresh_at: expires_at.saturating_sub(REFRESH_LEAD_MS + jitter),
    })
}

/// [0, 1) 的随机数（Node `Math.random()`）。
#[expect(
    clippy::disallowed_methods,
    reason = "platform 是加密强度随机数的入口：令牌刷新的抖动（README §4 的随机类别，F58）"
)]
fn random_unit() -> f64 {
    let mut bytes = [0u8; 4];
    if SystemRandom::new().fill(&mut bytes).is_err() {
        return 0.0;
    }
    f64::from(u32::from_le_bytes(bytes)) / (f64::from(u32::MAX) + 1.0)
}
