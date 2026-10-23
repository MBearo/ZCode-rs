// 语义复制自 apps/zcode-cli/packages/adapters/src/auth/coding-plan-api-key.ts、
// packages/shared/src/project-access-token{,-client}.ts（33a9541140；团队变体与 non-team 个人选择取 staging
// 699b62b8d6，工作线 F47）。改动见 platform.md §5.4：整条换取按账号与项目单飞；HTTP 经 net；时钟注入。
//! OAuth 登录的 Coding Plan 项目令牌（platform.md §5.4、§9.4）。
//!
//! 用 login token 换短期项目令牌（PAT）：找到项目位置（组织、项目、`zcode-api-key` 或团队的
//! `zcode-team-api-key`，存进 `credentials.json`）；再签发 PAT。login token 是 bigmodel 的 access token 或 zai 存好的
//! 业务 JWT（F47 照 App：请求期不再换业务 JWT，只有登录时换一次，[`ProjectTokens::business`]）。结果缓存在
//! 进程内，到刷新点（到期前 120 秒再减 0–60 秒抖动）前直接给出。
//!
//! 并发：状态在一个 std 锁里，锁不跨 await。同一 (family, 账号与项目, login token) 的并发调用共用一次换取
//! （`OnceCell`）；换取结束时在同一次加锁里写缓存并撤下自己，之后的调用走缓存。`clear` 让在途换取的
//! 结果作废（`project_token_scope_invalidated`）。

mod business;
pub(crate) mod http;
mod issue;
pub(crate) mod location;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::clock::Clock;
use rust_cli_net::Egress;
use sha2::{Digest, Sha256};
use tokio::sync::OnceCell;

pub use location::Location;
pub(crate) use location::TeamProject;

use crate::credentials::CredentialStore;
use business::BusinessSession;

/// 过期前多久刷新（毫秒）：项目令牌与 Z.ai 业务 JWT 相同（F55：以前 issue 与 business 各写一份）。
const REFRESH_LEAD_MS: u64 = 120_000;
/// 刷新失败后多久再试（毫秒），两种令牌相同。
const RETRY_BACKOFF_MS: u64 = 5_000;
use http::Http;

/// Coding Plan 的账号体系（`accountType`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Family {
    Zai,
    Bigmodel,
}

impl Family {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Zai => "zai",
            Self::Bigmodel => "bigmodel",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "zai" => Some(Self::Zai),
            "bigmodel" => Some(Self::Bigmodel),
            _ => None,
        }
    }

    /// 显示名（Node `providerName`）：目录项的供应商名（platform.md §9.3）与 TUI 的登录文字都用它，中英文相同。
    /// F58：以前 platform（`account::vendor`）与 tui（`i18n_account::provider_name`）各写一份。
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Zai => "Z.AI",
            Self::Bigmodel => "BigModel",
        }
    }
}

/// 换取失败。文本是 Node 的固定错误码：不带上游错误体、URL 或令牌。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    #[error("project_token_login_required")]
    LoginRequired,
    #[error("project_token_login_failed")]
    LoginFailed,
    #[error("project_token_scope_invalidated")]
    ScopeInvalidated,
    #[error("project_token_request_failed")]
    RequestFailed,
    #[error("project_token_invalid_response")]
    InvalidResponse,
    #[error("project_token_project_unavailable")]
    ProjectUnavailable,
    #[error("project_token_disabled")]
    Disabled,
}

/// 账号服务的地址与签发时上报的版本（入口按环境变量解析，platform.md §5.4、§5.6）。地址是 origin，
/// 不带末尾的 `/`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountOrigins {
    /// ZCode 服务的 API 根（`<origin>/api/v1`）：CLI OAuth 登录。
    pub zcode_api: String,
    /// Z.ai 业务与项目接口（`https://api.z.ai`）。
    pub zai_api: String,
    /// BigModel 项目接口（`https://bigmodel.cn`）。
    pub bigmodel_api: String,
    /// BigModel 团队套餐的管理页（App `buildBigModelCodingPlanTeamManageUrl({ ZCODE_ENV })`：只看 `ZCODE_ENV`，不跟随
    /// 地址的覆盖变量，F58）。z.ai 的管理页 App 写死，见 `team::manage_url`。
    pub bigmodel_team_plans: String,
    /// 签发项目令牌时的 `clientVersion`。
    pub client_version: String,
}

/// 一次换取的结果。
#[derive(Debug, Clone)]
pub struct Material {
    pub token: RedactedString,
    /// 项目位置（App `ProjectAccessTokenMaterial` 一起给出）。工作线 F50 起登录不再用它算 bigmodel 的身份，只有测试读。
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "只有测试读：换取结果照 App 带上项目位置")
    )]
    pub location: Location,
}

/// 一次换取的输入。`account` 是项目位置与缓存的范围（个人与团队都是 App 的 profile id，F50 之前个人是连接身份）；
/// `team` 是团队项目（个人为空）。
pub(crate) struct TokenRequest<'a> {
    pub family: Family,
    /// bigmodel 的 access token，或 zai 的业务 JWT。
    pub login_token: &'a str,
    pub account: &'a str,
    pub team: Option<&'a TeamProject>,
    /// 被模型服务拒绝（401）的 PAT 的指纹：只作废那一代。
    pub rejected: Option<&'a str>,
}

struct Issued {
    token: RedactedString,
    fingerprint: String,
    expires_at: u64,
    refresh_at: u64,
}

#[derive(Default)]
struct Account {
    location: Option<Location>,
    issued: Option<Issued>,
}

/// 一个 login token（zai 是业务 JWT，bigmodel 是 access token）下的位置与 PAT，按账号与项目
/// （[`location::scope_key`]）分开。token 变了整体换新。
struct Session {
    login_token: String,
    generation: u64,
    accounts: HashMap<String, Account>,
}

type Flight = Arc<OnceCell<Result<Material, TokenError>>>;
type FlightKey = (Family, String, String);

#[derive(Default)]
struct State {
    epoch: u64,
    next_generation: u64,
    business: Option<BusinessSession>,
    sessions: HashMap<(String, Family), Session>,
    flights: HashMap<FlightKey, Flight>,
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    // 持锁的代码不 panic、不 await；中毒时数据仍然一致，继续使用。
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 令牌指纹：只用于“作废哪一代”的比较，不作为身份，也不发出。
pub(crate) fn fingerprint(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// `loginToken.trim().replace(/^Bearer\s+/i, "")`。
fn login_token(raw: &str) -> String {
    let trimmed = raw.trim();
    match trimmed.get(..6) {
        Some(prefix)
            if prefix.eq_ignore_ascii_case("bearer")
                && trimmed[6..].starts_with(char::is_whitespace) =>
        {
            trimmed[6..].trim_start().to_owned()
        }
        _ => trimmed.to_owned(),
    }
}

pub(crate) struct ProjectTokens {
    http: Http,
    clock: Arc<dyn Clock>,
    origins: AccountOrigins,
    state: Mutex<State>,
}

impl ProjectTokens {
    pub fn new(egress: Arc<Egress>, clock: Arc<dyn Clock>, origins: AccountOrigins) -> Self {
        Self {
            http: Http::new(egress),
            clock,
            origins,
            state: Mutex::new(State::default()),
        }
    }

    fn origin(&self, family: Family) -> &str {
        match family {
            Family::Zai => &self.origins.zai_api,
            Family::Bigmodel => &self.origins.bigmodel_api,
        }
    }

    /// 清空全部缓存；在途换取的结果作废（凭据被删除或不再是 OAuth 登录时）。
    pub fn clear(&self) {
        let mut state = lock(&self.state);
        state.epoch += 1;
        state.business = None;
        state.sessions.clear();
        state.flights.clear();
    }

    /// 当前可用的 PAT：缓存命中时直接给出，否则加入（或发起）这个账号的换取。`store` 是项目位置的存放处（F50 追加
    /// 之前登录时为空：登录不再换令牌，存放处总是有）。
    pub async fn resolve(
        &self,
        request: TokenRequest<'_>,
        store: &CredentialStore,
    ) -> Result<Material, TokenError> {
        let login = login_token(request.login_token);
        if login.is_empty() {
            return Err(TokenError::LoginRequired);
        }
        let scope = location::scope_key(request.account, request.team);
        let key = (request.family, scope.clone(), fingerprint(&login));
        let (flight, epoch) = {
            let mut state = lock(&self.state);
            if let Some(rejected) = request.rejected {
                // 只作废被拒的那一代；迟到的 401 碰不到之后换好的 PAT。
                for account in state
                    .sessions
                    .values_mut()
                    .flat_map(|s| s.accounts.values_mut())
                {
                    if account
                        .issued
                        .as_ref()
                        .is_some_and(|i| i.fingerprint == rejected)
                    {
                        account.issued = None;
                    }
                }
            }
            if let Some(material) = self.cached(&state, request.family, &login, &scope) {
                return Ok(material);
            }
            let flight = state.flights.entry(key.clone()).or_default().clone();
            (flight, state.epoch)
        };
        flight
            .get_or_init(|| async {
                let input = issue::Input {
                    family: request.family,
                    login: &login,
                    account: request.account,
                    team: request.team,
                    scope: &scope,
                    epoch,
                };
                let result = self.exchange(input, store).await;
                let mut state = lock(&self.state);
                if state.flights.get(&key).is_some_and(|f| Arc::ptr_eq(f, &flight)) {
                    state.flights.remove(&key);
                }
                if let Err(error) = &result {
                    tracing::warn!(family = request.family.as_str(), code = %error, "project token");
                }
                result
            })
            .await
            .clone()
    }

    /// 缓存里能直接用的 PAT（会话、位置与 PAT 都还有效）。
    fn cached(&self, state: &State, family: Family, login: &str, scope: &str) -> Option<Material> {
        let now = self.clock.now_ms();
        let session = state
            .sessions
            .get(&(self.origin(family).to_owned(), family))
            .filter(|session| session.login_token == login)?;
        let account = session.accounts.get(scope)?;
        let issued = account.issued.as_ref()?;
        let location = account.location.clone()?;
        (now < issued.refresh_at.min(issued.expires_at)).then(|| Material {
            token: issued.token.clone(),
            location,
        })
    }

    /// 一次完整的换取（App `ProjectAccessTokenClient.resolve`）。修复线 F47：以前 zai 在这里先换业务 JWT、
    /// 被 `/api/biz/` 401 拒绝时重换一次（Node 独立 CLI）；现在存的就是业务 JWT（App），请求期不再换。
    async fn exchange(
        &self,
        input: issue::Input<'_>,
        store: &CredentialStore,
    ) -> Result<Material, TokenError> {
        self.check_epoch(input.epoch)?;
        self.resolve_with(input, store).await
    }

    fn check_epoch(&self, epoch: u64) -> Result<(), TokenError> {
        if lock(&self.state).epoch == epoch {
            Ok(())
        } else {
            Err(TokenError::ScopeInvalidated)
        }
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "flow_tests.rs"]
mod flow_tests;

#[cfg(test)]
#[path = "team_tests.rs"]
mod team_tests;
