// 语义复制自 apps/zcode-cli/packages/bootstrap/src/auth-login.ts、auth-login-persistence.ts（33a9541140）；OAuth 登录
// 写的键与值照 App 的轮询登录（packages/services/src/oauth/oauthService.ts 的 `pollPendingOAuth` 与
// `persistOAuthSession`，工作线 F50）。
//! 登录与退出（platform.md §5.6）：CLI OAuth 轮询登录、手工 key、退出。
//!
//! 打开浏览器与输出属于入口：[`crate::Platform::begin_login`] 拿到授权地址后交还入口，入口提示用户、
//! 打开浏览器，再调 [`PendingLogin::complete`] 轮询到结果并写文件。

mod legacy;
mod oauth;
mod persist;

use std::time::Duration;

use ring::rand::{SecureRandom, SystemRandom};
use rust_cli_base::secret::RedactedString;
use rust_cli_paths::StoragePath;
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use crate::Platform;
use crate::account::CodingPlanProvider;
use crate::token::{Family, ProjectTokens, TokenError};
use oauth::{OAuthClient, OAuthError, Poll};
pub(crate) use persist::identity_from_secret;

/// `/effort` 的缺省保存（tui-manage.md §2.2）：与登录写同一个字段，`auto`（没有档位）时不带 `options`。
pub(crate) async fn save_default_model(
    (file, legacy): (&rust_cli_paths::StoragePath, &rust_cli_paths::StoragePath),
    selection: &rust_cli_kernel_api::model::ModelSelection,
) -> Result<String, String> {
    persist::save_default_selection(
        (file, legacy),
        &selection.provider,
        &selection.model,
        selection.reasoning.as_deref(),
    )
    .await
    .map(|()| file.as_path().display().to_string())
    .map_err(|error| error.to_string())
}

const LOGIN_TIMEOUT_MS: u64 = 5 * 60 * 1000;
const MIN_POLL_INTERVAL_MS: u64 = 1_000;

/// 登录失败。文案与 Node `ZCodeCliLoginError` / `CliOAuthError` 相同。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LoginError {
    #[error("Authorization timed out. Please retry login.")]
    Timeout,
    #[error("Authorization failed. Please retry login.")]
    Failed,
    /// 用户取消（TUI 的 esc，platform.md §5.6「取消」）：轮询停下，不写任何文件。
    #[error("Login cancelled.")]
    Cancelled,
    /// init / poll 的错误（`CliOAuthError` 的文案或网络错误）。
    #[error("{0}")]
    OAuth(String),
    /// 用 access token 换项目令牌失败（Node 的错误码）。
    #[error("{0}")]
    Token(TokenError),
    #[error("Login succeeded but writing credentials failed.")]
    CredentialWrite(String),
    /// 个人配置写入失败；`configure_api_key` 时 Node 不包装，原因直接给出。
    #[error("Login succeeded but updating ZCode config failed.")]
    ConfigUpdate(String),
    #[error("API key must not be empty.")]
    EmptyApiKey,
    /// 个人配置还不存在，旧 CLI 用户文件有 Node 要导入的 provider 或默认模型：不建文件，留给 Node 导入
    /// （修复线 F2，platform.md §2.4）。
    #[error(
        "{file} does not exist yet and {legacy} has providers that ZCode imports when it creates it; start the Node CLI or the App once, then try again."
    )]
    LegacyImportPending { file: String, legacy: String },
    /// 内置配置没有给这个 family 声明唯一的个人 Coding Plan provider。
    #[error("ZCode Built-in Config 必须为 {0} 声明唯一 Individual Coding Plan Provider")]
    NoProvider(&'static str),
    /// 凭据文件读不出或损坏（退出、手工 key）。
    #[error("{0}")]
    Credentials(String),
}

/// 登录成功（OAuth）。
#[derive(Debug, Clone)]
pub struct LoginOutcome {
    pub family: Family,
    /// `user_id`、`name`、`email`、`avatar`（有的话）。
    pub user: Map<String, Value>,
    /// `<provider>/<model>`。
    pub model: String,
    pub credentials: StoragePath,
    pub config: StoragePath,
}

impl LoginOutcome {
    /// 提示里的用户名：`name`，其次 `email`，其次 `user_id`。
    pub fn user_label(&self) -> Option<&str> {
        ["name", "email", "user_id"]
            .into_iter()
            .filter_map(|key| self.user.get(key).and_then(Value::as_str))
            .find(|label| !label.is_empty())
    }
}

/// 手工 key 配置成功。
#[derive(Debug, Clone)]
pub struct SetupOutcome {
    pub family: Family,
    pub model: String,
    pub config: StoragePath,
}

/// 拿到授权地址、还没轮询的登录。
pub struct PendingLogin<'a> {
    platform: &'a Platform,
    client: OAuthClient,
    family: Family,
    provider: CodingPlanProvider,
    poll_token: String,
    flow_id: String,
    interval_ms: u64,
    deadline_ms: u64,
    authorize_url: String,
}

impl PendingLogin<'_> {
    pub fn authorize_url(&self) -> &str {
        &self.authorize_url
    }

    /// 轮询到结果（zai 再换业务 JWT），写凭据与个人配置。
    pub async fn complete(self) -> Result<LoginOutcome, LoginError> {
        self.complete_until(&CancellationToken::new()).await
    }

    /// 同 [`complete`](Self::complete)，`cancel` 被取消时停止轮询并返回 [`LoginError::Cancelled`]。取消只在
    /// 每次轮询之前与两次轮询之间生效（取消是一条消息，不丢弃在途的请求）；授权完成后的换取与写文件不可
    /// 取消，免得留下写了一半的文件。
    pub async fn complete_until(
        self,
        cancel: &CancellationToken,
    ) -> Result<LoginOutcome, LoginError> {
        let ready = self.poll_until_ready(cancel).await?;
        let platform = self.platform;
        let tokens = ProjectTokens::new(
            platform.services.egress.clone(),
            platform.services.clock.clone(),
            platform.services.origins.clone(),
        );
        // zai：OAuth token 先换业务 JWT，存的是业务 JWT（App `normalizePolledTokenSet`，修复线 F47；以前按 Node
        // 存原始 OAuth token、每次请求再换）。bigmodel 存 access token 本身。
        let login_token = match self.family {
            Family::Zai => {
                tokens
                    .business(ready.access_token.expose())
                    .await
                    .map_err(LoginError::Token)?
                    .token
            }
            Family::Bigmodel => ready.access_token.clone(),
        };
        // 修复线 F50 追加（用户决定 2026-10-21，照 App）：以前照 Node 独立 CLI 在这里用个人项目换一次令牌验证账号，
        // 只有团队项目的账号报 `project_token_project_unavailable`、登录失败；App 的轮询登录不换，个人套餐能不能用由
        // 取目录时的订阅查询决定（platform.md §5.6、§5.7）。
        let writes = app_login_writes(self.family, &ready, &login_token);
        persist::save_login(&platform.credentials, &self.provider, &writes)
            .await
            .map_err(|error| LoginError::CredentialWrite(error.to_string()))?;
        platform.save_selection(&self.provider).await?;
        Ok(LoginOutcome {
            family: self.family,
            user: ready.user,
            model: format!("{}/{}", self.provider.provider, self.provider.model),
            credentials: platform.credentials.file().clone(),
            config: platform.personal_config.clone(),
        })
    }

    async fn poll_until_ready(
        &self,
        cancel: &CancellationToken,
    ) -> Result<oauth::Ready, LoginError> {
        let clock = &self.platform.services.clock;
        while clock.now_ms() < self.deadline_ms {
            if cancel.is_cancelled() {
                return Err(LoginError::Cancelled);
            }
            // 修复线 F58：轮询请求与取消竞争（Node `waitWithAbort`）。以前只在两次轮询之间检查取消，请求挂住时 esc
            // 不起作用（请求也没有单次超时）。
            let polled = tokio::select! {
                () = cancel.cancelled() => return Err(LoginError::Cancelled),
                polled = self.client.poll(&self.flow_id, &self.poll_token) => polled,
            };
            let data = match polled {
                Ok(data) => data,
                // 与 App 的轮询相同：临时的网络与服务端错误按服务端给的间隔重试。
                Err(error) if error.transient() => Poll::Pending,
                Err(error) => return Err(LoginError::OAuth(error.message)),
            };
            let now = clock.now_ms();
            if now >= self.deadline_ms {
                break;
            }
            match data {
                Poll::Ready(ready) => return Ok(*ready),
                Poll::Failed => return Err(LoginError::Failed),
                Poll::Pending => {}
            }
            let wait = self.interval_ms.min(self.deadline_ms.saturating_sub(now));
            tokio::select! {
                () = cancel.cancelled() => return Err(LoginError::Cancelled),
                () = tokio::time::sleep(Duration::from_millis(wait)) => {}
            }
        }
        Err(LoginError::Timeout)
    }
}

impl Platform {
    /// 个人 Coding Plan provider（Node `resolveStandaloneCodingPlanProvider`：必须唯一）。
    async fn coding_plan(&self, family: Family) -> Result<CodingPlanProvider, LoginError> {
        let builtin = self
            .registry
            .builtin_config()
            .await
            .map_err(|_| LoginError::NoProvider(family.as_str()))?;
        let mut matches: Vec<CodingPlanProvider> = crate::account::coding_plan_providers(&builtin)
            .into_iter()
            .filter(|candidate| candidate.family == family.as_str())
            .collect();
        if matches.len() != 1 {
            return Err(LoginError::NoProvider(family.as_str()));
        }
        matches.pop().ok_or(LoginError::NoProvider(family.as_str()))
    }

    /// 登录后把这个 provider 存成缺省。旧 CLI 用户文件还等着 Node 导入时不写（凭据已保存，登录照样成功，记警告；
    /// 修复线 F2）。
    async fn save_selection(&self, provider: &CodingPlanProvider) -> Result<(), LoginError> {
        match persist::save_default_selection(
            (&self.personal_config, &self.legacy_config),
            &provider.provider,
            &provider.model,
            None,
        )
        .await
        {
            Err(error @ LoginError::LegacyImportPending { .. }) => {
                tracing::warn!(%error, "the default model selection is left to the legacy import");
                Ok(())
            }
            other => other,
        }
    }

    /// CLI OAuth 登录的第一步：向 ZCode 服务要授权地址。
    pub async fn begin_login(&self, family: Family) -> Result<PendingLogin<'_>, LoginError> {
        let provider = self.coding_plan(family).await?;
        let client = OAuthClient {
            egress: self.services.egress.clone(),
            base_url: self.services.origins.zcode_api.clone(),
            provider: family.as_str(),
        };
        let poll_token = random_hex(32).map_err(|_| LoginError::OAuth("random".to_owned()))?;
        let started = self.services.clock.now_ms();
        let init = client
            .init(&poll_token)
            .await
            .map_err(|error: OAuthError| LoginError::OAuth(error.message))?;
        let expires_ms = (init.expires_at * 1000.0).max(0.0) as u64;
        let deadline_ms = (started + LOGIN_TIMEOUT_MS).min(expires_ms);
        if deadline_ms <= self.services.clock.now_ms() {
            return Err(LoginError::Timeout);
        }
        Ok(PendingLogin {
            platform: self,
            client,
            family,
            provider,
            poll_token,
            flow_id: init.flow_id,
            interval_ms: ((init.poll_interval_sec * 1000.0) as u64).max(MIN_POLL_INTERVAL_MS),
            deadline_ms,
            authorize_url: init.authorize_url,
        })
    }

    /// 手工 key 登录（Node `configureCodingPlanApiKey`）：不验证 key。
    pub async fn configure_api_key(
        &self,
        family: Family,
        api_key: &RedactedString,
    ) -> Result<SetupOutcome, LoginError> {
        let key = api_key.expose().trim();
        if key.is_empty() {
            return Err(LoginError::EmptyApiKey);
        }
        let provider = self.coding_plan(family).await?;
        persist::save_manual(
            &self.credentials,
            &provider,
            &identity_from_secret(key),
            key,
        )
        .await
        .map_err(|error| LoginError::Credentials(error.to_string()))?;
        self.save_selection(&provider).await?;
        Ok(SetupOutcome {
            family,
            model: format!("{}/{}", provider.provider, provider.model),
            config: self.personal_config.clone(),
        })
    }

    /// 退出 Coding Plan 账号（Node `logoutZCodeCli`）。返回凭据文件的位置。
    pub async fn logout(&self) -> Result<StoragePath, LoginError> {
        let builtin = self
            .registry
            .builtin_config()
            .await
            .map_err(LoginError::Credentials)?;
        let providers = crate::account::coding_plan_providers(&builtin);
        persist::forget_connections(&self.credentials, &providers)
            .await
            .map_err(|error| LoginError::Credentials(error.to_string()))?;
        self.auth.clear();
        Ok(self.credentials.file().clone())
    }
}

/// OAuth 登录写的凭据（App 轮询登录的 `persistOAuthSession`：`saveTokenSet`、`saveUserProfile`、`setActiveProvider`，
/// 按它的顺序；值为空的键删除）。修复线 F50：以前照 Node 独立 CLI 写，zai 的 user_info 只留四个字段并去空白，
/// bigmodel 的 user_info 用 `key-` + sha256(JSON [组织, 项目]) 作 id，另写 `account-provider:<id>:identity` 与
/// `auth-source`；App 读不出同一个身份，项目位置的键也与 App 不同。
fn app_login_writes(
    family: Family,
    ready: &oauth::Ready,
    login_token: &RedactedString,
) -> Vec<(&'static str, Option<String>)> {
    let token = Some(ready.token.expose().to_owned());
    let access = Some(login_token.expose().to_owned());
    match family {
        // App `saveUserProfile`：zai 存后端原始用户对象（`rawProfile`）；没有 refresh token 时删。
        Family::Zai => vec![
            ("oauth:zai:access_token", access),
            ("oauth:zai:refresh_token", None),
            ("zcodejwttoken", token),
            (
                "oauth:zai:user_info",
                Some(rust_cli_base::js::json::stringify(&Value::Object(
                    ready.raw_user.clone(),
                ))),
            ),
            ("oauth:active_provider", Some("zai".to_owned())),
        ],
        Family::Bigmodel => vec![
            ("oauth:bigmodel:access_token", access),
            (
                "oauth:bigmodel:refresh_token",
                ready.refresh_token.as_ref().map(|t| t.expose().to_owned()),
            ),
            ("zcodejwttoken", token),
            (
                "oauth:bigmodel:user_info",
                Some(rust_cli_base::js::json::stringify(&app_profile(ready))),
            ),
            ("oauth:active_provider", Some("bigmodel".to_owned())),
        ],
    }
}

/// App 轮询登录的 bigmodel profile（`pollPendingOAuth` 的 `profile` 经 `withProviderProfileSchema`）：
/// `{id: user_id, username, displayName, [avatarUrl], rawProfile: {…user, zcodeProfileSchemaVersion: 2}}`。没有版本号时
/// App 启动会用 userinfo 接口把 id 换成 `customerNumber`（`refreshLegacyBigModelCachedProfile`），项目位置的键随之改变。
fn app_profile(ready: &oauth::Ready) -> Value {
    let user_id = ready.user["user_id"].as_str().unwrap_or_default();
    let trimmed = |name: &str| {
        ready
            .raw_user
            .get(name)
            .and_then(Value::as_str)
            .map(rust_cli_base::js::string::trim)
            .filter(|text| !text.is_empty())
    };
    let username = trimmed("name")
        .or_else(|| trimmed("email"))
        .unwrap_or(user_id);
    let mut profile = Map::new();
    profile.insert("id".to_owned(), json!(user_id));
    profile.insert("username".to_owned(), json!(username));
    profile.insert("displayName".to_owned(), json!(username));
    if let Some(avatar) = trimmed("avatar") {
        profile.insert("avatarUrl".to_owned(), json!(avatar));
    }
    let mut raw = ready.raw_user.clone();
    raw.shift_remove("zcodeProfileMigrationRetryAfter");
    raw.insert("zcodeProfileSchemaVersion".to_owned(), json!(2));
    profile.insert("rawProfile".to_owned(), Value::Object(raw));
    Value::Object(profile)
}

/// `randomBytes(n).toString("hex")`。
#[expect(
    clippy::disallowed_methods,
    reason = "platform 是加密强度随机数的入口：OAuth 轮询的令牌（README §4 的随机类别，F58）"
)]
fn random_hex(bytes: usize) -> Result<String, ring::error::Unspecified> {
    let mut buffer = vec![0u8; bytes];
    SystemRandom::new().fill(&mut buffer)?;
    Ok(buffer.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
#[path = "flow_tests.rs"]
mod flow_tests;

#[cfg(test)]
#[path = "cancel_tests.rs"]
mod cancel_tests;
