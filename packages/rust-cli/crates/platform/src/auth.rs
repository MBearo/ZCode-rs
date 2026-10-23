// 语义复制自 apps/zcode-cli/packages/bootstrap/src/app/standalone-account-provider-runtime.ts（33a9541140）的
// `createStandaloneProviderRuntimeHeadersPort`（手工 key）；OAuth 的个人与团队分支取 packages/services/src/
// model-provider/accountProviderRequestAuthService.ts 与 accountProviderTeamPlanRequestKey.ts（staging 699b62b8d6，
// 工作线 F47；个人分支 F50）。改动：`expectedAccountScope` 不经模型适配器回传，改由本进程记下的“令牌指纹 → 账号范围”
// 给出（platform.md §5.3）；团队项目取自模型选择，不读 App 的当前连接（工作线 F49，App `resolveCurrentAccountAccess`
// 读 `setting.json`）。
//! 账号模型的每请求凭据（platform.md §5.3、§9.5）：手工 key，或 OAuth access token 换来的项目令牌（个人或团队）。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::model::{ModelConnection, ModelError};
use sha2::{Digest, Sha256};

use crate::account::{self, OAuthLogin, RequestAuth, profile_id};
use crate::credentials::{CredentialStore, Credentials};
use crate::token::{Family, ProjectTokens, TeamProject, TokenError, TokenRequest, fingerprint};

/// 记下的“令牌指纹 → 账号范围”的条数上限（令牌每小时级才换一次）。
const ISSUED_LIMIT: usize = 32;

pub(crate) struct AccountAuth {
    credentials: CredentialStore,
    /// 与个人订阅查询共用（`personal/`）：取目录时换到的项目令牌，第一次请求直接用。
    tokens: Arc<ProjectTokens>,
    /// 本进程交出过的项目令牌：(指纹, 账号范围)。
    issued: Mutex<VecDeque<(String, String)>>,
}

fn authentication(error: TokenError) -> ModelError {
    let hint = match error {
        TokenError::LoginRequired | TokenError::LoginFailed => "run `zcode login` again",
        TokenError::ScopeInvalidated => "the account changed during the request",
        TokenError::Disabled => "project tokens are disabled for this account",
        TokenError::ProjectUnavailable => "the account has no usable project",
        TokenError::RequestFailed | TokenError::InvalidResponse => {
            "exchanging the login for a project token failed"
        }
    };
    ModelError::Authentication {
        message: format!("{error}: {hint}"),
    }
}

impl AccountAuth {
    pub fn new(credentials: CredentialStore, tokens: Arc<ProjectTokens>) -> Self {
        Self {
            credentials,
            tokens,
            issued: Mutex::new(VecDeque::new()),
        }
    }

    /// 清空项目令牌的缓存（退出后）。
    pub fn clear(&self) {
        self.tokens.clear();
    }

    /// 这次请求的凭据。`team` 是模型选择里的团队项目（团队 provider 才有）；`rejected` 是刚被模型服务拒绝（401）
    /// 的那份。
    pub async fn credential(
        &self,
        (provider, family, mode): (&str, &str, &str),
        team: Option<&ModelConnection>,
        rejected: Option<&RedactedString>,
    ) -> Result<RedactedString, ModelError> {
        let read = |error: crate::credentials::CredentialError| ModelError::Authentication {
            message: error.to_string(),
        };
        let rejected = rejected.map(|token| fingerprint(token.expose()));
        let issued_scope = rejected.as_deref().and_then(|print| self.scope_of(print));
        if mode == account::TEAM_CODING_PLAN {
            return self
                .team((provider, family), team, rejected.as_deref(), issued_scope)
                .await;
        }
        let credentials = self.credentials.read().await.map_err(read)?;
        let auth = match account::request_auth(&credentials, provider, family, mode) {
            Ok(auth) => auth,
            Err(error) => {
                // 登录状态没了：在途与缓存的项目令牌都不能再交出。
                self.tokens.clear();
                return Err(error);
            }
        };
        let before = match auth {
            // 被拒的是本进程发出的项目令牌、而现在是手工 key：账号已切换，不拿另一个账号重发。
            RequestAuth::Manual(_) if issued_scope.is_some() => {
                return Err(authentication(TokenError::ScopeInvalidated));
            }
            RequestAuth::Manual(key) => return Ok(key),
            RequestAuth::OAuth(login) => login,
        };
        let Some(family) = Family::parse(family) else {
            return Err(authentication(TokenError::LoginRequired));
        };
        let scope = individual_scope(family, &before);
        if rejected.is_some() && issued_scope.as_deref() != Some(scope.as_str()) {
            return Err(authentication(TokenError::ScopeInvalidated));
        }
        // 修复线 F50：以前没有来源时先持锁补写 `auth-source = oauth`（Node 独立 CLI）；现在身份取自 App 写的
        // user_info，请求期不替 App 写键。
        let request = TokenRequest {
            family,
            login_token: before.access_token.expose(),
            account: &before.identity,
            team: None,
            rejected: rejected.as_deref(),
        };
        let material = self
            .tokens
            .resolve(request, &self.credentials)
            .await
            .map_err(authentication)?;
        // 换取期间凭据变了（退出、换账号）：迟到的令牌不能交给新登录态（App 换证前后核对范围）。
        let current = self.credentials.read().await.map_err(read)?;
        let after = match account::request_auth(&current, provider, family.as_str(), mode) {
            Ok(RequestAuth::OAuth(login)) => Some(login),
            Ok(RequestAuth::Manual(_)) | Err(_) => None,
        };
        let unchanged = after.as_ref().is_some_and(|login| {
            login.identity == before.identity && individual_scope(family, login) == scope
        });
        if !unchanged {
            if after.is_none_or(|login| login.access_token != before.access_token) {
                self.tokens.clear();
            }
            return Err(authentication(TokenError::ScopeInvalidated));
        }
        self.remember(fingerprint(material.token.expose()), scope);
        Ok(material.token)
    }

    /// 团队项目的凭据（App `AccountProviderRequestAuthService.resolveCurrent` 的团队分支，platform.md §9.5）。团队
    /// 项目取自选择（F49；F47 取 App 的当前连接，不是当前连接时报不可用）。
    async fn team(
        &self,
        (provider, family): (&str, &str),
        team: Option<&ModelConnection>,
        rejected: Option<&str>,
        issued_scope: Option<String>,
    ) -> Result<RedactedString, ModelError> {
        // App `AccountRequestCredentialUnavailableError` 的原文：没有登录、没有 profile id，或选择没带团队项目。
        let unavailable = || ModelError::Authentication {
            message: format!("Account request credential is unavailable: {provider}"),
        };
        let family = Family::parse(family).ok_or_else(unavailable)?;
        let team = team.ok_or_else(unavailable)?;
        let before = self
            .team_access(family, team)
            .await?
            .ok_or_else(unavailable)?;
        if rejected.is_some() && issued_scope.as_deref() != Some(before.scope.as_str()) {
            return Err(authentication(TokenError::ScopeInvalidated));
        }
        let request = TokenRequest {
            family,
            login_token: before.token.expose(),
            account: &before.identity,
            team: Some(&before.team),
            rejected,
        };
        let material = self
            .tokens
            .resolve(request, &self.credentials)
            .await
            .map_err(authentication)?;
        // 换证 IO 前后校验原请求身份，避免 401 重试串用新账号（App 相同）。
        let after = self.team_access(family, team).await?;
        if after.as_ref().map(|access| &access.scope) != Some(&before.scope) {
            if after.is_none_or(|access| access.token != before.token) {
                self.tokens.clear();
            }
            return Err(authentication(TokenError::ScopeInvalidated));
        }
        self.remember(fingerprint(material.token.expose()), before.scope);
        Ok(material.token)
    }

    /// 这个团队项目的账号（App `resolveCurrentAccountAccess` + `#scope`，团队项目取自选择）；没有登录时为空。
    async fn team_access(
        &self,
        family: Family,
        team: &ModelConnection,
    ) -> Result<Option<TeamAccess>, ModelError> {
        let credentials =
            self.credentials
                .read()
                .await
                .map_err(|error| ModelError::Authentication {
                    message: error.to_string(),
                })?;
        Ok(TeamAccess::read(&credentials, family, team))
    }

    fn scope_of(&self, print: &str) -> Option<String> {
        let issued = self.issued.lock().unwrap_or_else(PoisonError::into_inner);
        issued
            .iter()
            .find(|(known, _)| known == print)
            .map(|(_, scope)| scope.clone())
    }

    fn remember(&self, print: String, scope: String) {
        let mut issued = self.issued.lock().unwrap_or_else(PoisonError::into_inner);
        if issued.iter().any(|(known, _)| *known == print) {
            return;
        }
        if issued.len() == ISSUED_LIMIT {
            issued.pop_front();
        }
        issued.push_back((print, scope));
    }
}

/// 一次团队请求的账号：profile id、access token、团队项目与账号范围。
struct TeamAccess {
    identity: String,
    token: RedactedString,
    team: TeamProject,
    scope: String,
}

impl TeamAccess {
    fn read(credentials: &Credentials, family: Family, team: &ModelConnection) -> Option<Self> {
        let identity = profile_id(credentials, family)?;
        let token = credentials.present(account::oauth_key(family.as_str()))?;
        let jwt = credentials.present(account::ZCODE_JWT_KEY);
        // BigModel /api/biz 只接受登录 access token，不能用旧版本误存的 ZCode JWT（App 相同）。
        if family == Family::Bigmodel && jwt.as_ref() == Some(&token) {
            return None;
        }
        let project = serde_json::json!([team.organization_id, team.project_id, team.product_id]);
        let scope = app_scope(
            family,
            account::TEAM_CODING_PLAN,
            project,
            &token,
            jwt.as_ref(),
        );
        Some(Self {
            identity,
            token,
            team: TeamProject {
                organization_id: team.organization_id.clone(),
                project_id: team.project_id.clone(),
            },
            scope,
        })
    }
}

/// App `AccountProviderRequestAuthService.#scope`：sha256(JSON [family, planKind, 团队项目 | null, access token,
/// zcodejwttoken 或 null])，两个 token 去空白。
fn app_scope(
    family: Family,
    plan: &str,
    team: serde_json::Value,
    token: &RedactedString,
    jwt: Option<&RedactedString>,
) -> String {
    let text = rust_cli_base::js::json::stringify(&serde_json::json!([
        family.as_str(),
        plan,
        team,
        token.expose(),
        jwt.map(RedactedString::expose),
    ]));
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 个人 OAuth 的账号范围（App `#scope` 的个人分支，F50）。以前照 Node 是 sha256(JSON [provider, family, 身份,
/// access token])：zcodejwttoken 变了不算换账号。
fn individual_scope(family: Family, login: &OAuthLogin) -> String {
    app_scope(
        family,
        account::INDIVIDUAL_CODING_PLAN,
        serde_json::Value::Null,
        &login.access_token,
        login.zcode_jwt.as_ref(),
    )
}
