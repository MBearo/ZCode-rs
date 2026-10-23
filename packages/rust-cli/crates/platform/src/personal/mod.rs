// 语义复制自 packages/services/src/model-provider/accountProviderConnectionResolver.ts 的个人分支（身份 → 个人项目
// 令牌 → 订阅查询）与 packages/provider/src/account-provider-resolution.ts 的投影（unknown 沿用上次），feat/rust-cli
// 2827169cbc，工作线 F50。
//! 个人 Coding Plan 的授权（platform.md §5.2、§5.7）：照 App，OAuth 登录（App 或 `zcode login` 写的）的个人套餐要
//! 有 profile id，再用个人项目令牌查个人订阅，available 才可用。手工 key 不经过这里（`account.rs`）。
//!
//! 结论按 provider 记在进程内（profile id、access token 指纹、结论）。available / unavailable 之后直接用，profile id
//! 或 token 变了才重查；unknown 不记结论，entitled 沿用上次同一 profile id 的值（第一次 false），下一次重建快照时
//! 再查；profile id 没了（退出）时删掉记录。查询途中账号变了由 registry 丢弃这一轮，所以结论只在发布前经
//! [`PersonalPlans::commit`] 记下（与 `team/` 相同）。
//!
//! 换到的项目令牌留在与请求期共用的缓存里（`token/`），第一次请求直接用。并发：状态在一个 std 锁里，锁不跨 await；
//! 同一 (provider, profile id, token) 的并发查询共用一次（`OnceCell`）。

mod check;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rust_cli_base::secret::RedactedString;
use rust_cli_net::Egress;
use tokio::sync::OnceCell;

use crate::credentials::CredentialStore;
use crate::team::{Availability, Reason};
use crate::token::http::Http;
use crate::token::{AccountOrigins, Family, ProjectTokens, TokenRequest, fingerprint};

/// 一个个人 Coding Plan provider 的 OAuth 账号输入（来源是 manual 的不在这里，platform.md §5.2）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PersonalAccount {
    pub provider: String,
    pub family: Family,
    /// App 的 profile id（`oauth:<family>:user_info`）。
    pub identity: Option<String>,
    /// `oauth:<family>:access_token`（zai 是业务 JWT）。
    pub token: Option<RedactedString>,
}

/// 这一轮的结论（发布前经 [`PersonalPlans::commit`] 记下）。
#[derive(Debug, Clone, Copy)]
pub(crate) struct Check {
    availability: Availability,
    /// 这一轮发过请求。
    pub networked: bool,
}

struct Recorded {
    identity: String,
    token: String,
    availability: Availability,
    entitled: bool,
}

type Flight = Arc<OnceCell<Availability>>;
type FlightKey = (String, String, String);

#[derive(Default)]
struct State {
    recorded: HashMap<String, Recorded>,
    flights: HashMap<FlightKey, Flight>,
}

pub(crate) struct PersonalPlans {
    http: Http,
    origins: AccountOrigins,
    tokens: Arc<ProjectTokens>,
    credentials: CredentialStore,
    state: Mutex<State>,
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    // 持锁的代码不 panic、不 await；中毒时数据仍然一致，继续使用。
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

impl PersonalPlans {
    /// `tokens` 与请求期共用（同一账号的项目令牌只换一次）；项目位置写进 `credentials`（App 能读，§5.4）。
    pub fn new(
        egress: Arc<Egress>,
        origins: AccountOrigins,
        tokens: Arc<ProjectTokens>,
        credentials: CredentialStore,
    ) -> Self {
        Self {
            http: Http::new(egress),
            origins,
            tokens,
            credentials,
            state: Mutex::new(State::default()),
        }
    }

    fn origin(&self, family: Family) -> &str {
        match family {
            Family::Zai => &self.origins.zai_api,
            Family::Bigmodel => &self.origins.bigmodel_api,
        }
    }

    /// 这一轮的结论：没有 profile id 是未连接（不联网）；有 profile id、没有 access token 是 unknown（App 换不到项目
    /// 令牌时 `authorization` 为空）；记下的确定结论直接用；其余换项目令牌再查订阅。
    pub async fn refresh(&self, account: &PersonalAccount) -> Check {
        let offline = |availability| Check {
            availability,
            networked: false,
        };
        let Some(identity) = account.identity.as_deref() else {
            return offline(Availability::Unavailable(Reason::NotEntitled));
        };
        let Some(token) = account.token.as_ref() else {
            return offline(Availability::Unknown);
        };
        let print = fingerprint(token.expose());
        if let Some(known) = self.known(&account.provider, identity, &print) {
            return offline(known);
        }
        let key = (account.provider.clone(), identity.to_owned(), print);
        let flight = lock(&self.state)
            .flights
            .entry(key.clone())
            .or_default()
            .clone();
        let availability = *flight
            .get_or_init(|| async {
                let availability = self.query(account.family, identity, token).await;
                let mut state = lock(&self.state);
                if state
                    .flights
                    .get(&key)
                    .is_some_and(|f| Arc::ptr_eq(f, &flight))
                {
                    state.flights.remove(&key);
                }
                availability
            })
            .await;
        Check {
            availability,
            networked: true,
        }
    }

    /// 换个人项目令牌（账号 = profile id，位置写进凭据文件），再查订阅。换取失败（任何错误码）是 unknown：App 的
    /// `loadCodingPlanApiKey(...).catch(() => null)` 之后没有项目令牌。
    async fn query(&self, family: Family, identity: &str, token: &RedactedString) -> Availability {
        let request = TokenRequest {
            family,
            login_token: token.expose(),
            account: identity,
            team: None,
            rejected: None,
        };
        let availability = match self.tokens.resolve(request, &self.credentials).await {
            Ok(material) => {
                check::subscription(&self.http, self.origin(family), material.token.expose()).await
            }
            Err(_) => Availability::Unknown,
        };
        tracing::info!(family = family.as_str(), result = ?availability, "individual plan");
        availability
    }

    /// 记下的确定结论（同一 profile id 与 token）。
    fn known(&self, provider: &str, identity: &str, print: &str) -> Option<Availability> {
        let state = lock(&self.state);
        let recorded = state.recorded.get(provider)?;
        (recorded.identity == identity
            && recorded.token == print
            && recorded.availability != Availability::Unknown)
            .then_some(recorded.availability)
    }

    /// 发布前记下这一轮，给出 entitled：available true、unavailable false、unknown 沿用上次同一 profile id 的值（第一次
    /// false；换了 profile id 不沿用，App `resetPrevious`）。没有 profile id 时删掉记录。
    pub fn commit(&self, account: &PersonalAccount, check: Check) -> bool {
        let mut state = lock(&self.state);
        let Some(identity) = account.identity.clone() else {
            state.recorded.remove(&account.provider);
            return false;
        };
        let previous = state
            .recorded
            .get(&account.provider)
            .filter(|recorded| recorded.identity == identity)
            .is_some_and(|recorded| recorded.entitled);
        let entitled = match check.availability {
            Availability::Available => true,
            Availability::Unavailable(_) => false,
            Availability::Unknown => previous,
        };
        let token = account
            .token
            .as_ref()
            .map(|token| fingerprint(token.expose()))
            .unwrap_or_default();
        state.recorded.insert(
            account.provider.clone(),
            Recorded {
                identity,
                token,
                availability: check.availability,
                entitled,
            },
        );
        entitled
    }
}

#[cfg(test)]
#[path = "platform_tests.rs"]
mod platform_tests;
