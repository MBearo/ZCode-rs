// 语义复制自 packages/services/src/model-provider/accountProviderConnectionResolver.ts 的 last-known-good 与
// packages/provider/src/account-provider-resolution.ts 的投影（staging 699b62b8d6，工作线 F47）。工作线 F49：不再
// 只查 App 的当前连接，按团队列表（`list.rs`，App 的 pricing + customerInfo）逐个项目查。
//! Team Plan 的团队列表与权益（platform.md §9.1、§9.2）。
//!
//! 结果记在进程内：列表按 (provider, profile id, access token 指纹)，权益按 (provider, 团队项目) 记下 profile id、
//! token 指纹与结论。确定的结果（列出成功；available / unavailable）之后直接用，profile id 或 token 变了才重查；
//! unknown 不记，沿用上次同范围的值（列表第一次是空、权益第一次 false），下一次重建快照时再查。查询途中账号变了
//! 由 registry 丢弃这一轮（§9.7），所以结果只在发布前经 [`TeamPlans::commit`] 记下。
//!
//! 并发：状态在一个 std 锁里，锁不跨 await；同一账号的并发刷新共用一次（`OnceCell`）；一轮里 pricing 与
//! customerInfo 同时发，各项目的订阅详情同时发（第一次取目录最多 2 × 15 秒，D-F49-1）。

mod check;
mod list;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use futures_util::future::join_all;
use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::model::ModelConnection;
use rust_cli_net::Egress;
use tokio::sync::OnceCell;

pub(crate) use check::successful_data;
pub(crate) use list::TeamPlan;

use crate::token::http::Http;
use crate::token::{AccountOrigins, Family, fingerprint};

/// 一次查询的结论（App `CodingPlanAvailabilityResult` 的团队部分；不可用的原因保留细分，给提示用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Availability {
    Available,
    Unavailable(Reason),
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reason {
    /// 401 / 403。
    AuthFailed,
    /// 不在所选项目里，或没有订阅。
    NotEntitled,
    /// `status == "EXPIRED"`。
    Expired,
    /// `status == "EFFECTIVE"` 且 `memberGrantStatus == "UNASSIGNED"`。
    Unassigned,
}

/// 一个团队 provider 的账号输入（从 `credentials.json` 读，platform.md §9.1）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TeamAccount {
    pub provider: String,
    pub family: Family,
    pub identity: Option<String>,
    pub token: Option<RedactedString>,
    pub zcode_jwt: Option<RedactedString>,
}

impl TeamAccount {
    /// 已连接时的 access token（App：没有 token、bigmodel 的 token 等于 zcodejwttoken、没有 profile id 都是未连接）。
    fn connected(&self) -> Option<&str> {
        let token = self.token.as_ref()?;
        let stale = self.family == Family::Bigmodel
            && self
                .zcode_jwt
                .as_ref()
                .is_some_and(|jwt| jwt.expose() == token.expose());
        (!stale && self.identity.is_some()).then_some(token.expose())
    }

    fn token_print(&self) -> String {
        self.token
            .as_ref()
            .map(|token| fingerprint(token.expose()))
            .unwrap_or_default()
    }
}

/// 这一轮一个团队 provider 的结论（发布前经 [`TeamPlans::commit`] 记下）。
#[derive(Debug, Clone, Default)]
pub(crate) struct Round {
    /// 新列出的团队项目；用了记下的列表或列表读取失败（unknown）时为 None。
    fresh: Option<Vec<TeamPlan>>,
    /// 这一轮的列表（新列出的、记下的或沿用的）里每个项目的结论，按列表顺序。
    plans: Vec<(TeamPlan, Availability)>,
    /// 这一轮发过请求。
    pub networked: bool,
}

struct RecordedList {
    identity: Option<String>,
    token: String,
    plans: Vec<TeamPlan>,
}

struct Recorded {
    identity: Option<String>,
    token: String,
    availability: Availability,
    entitled: bool,
}

type Flight = Arc<OnceCell<Round>>;
type FlightKey = (String, Option<String>, String);

#[derive(Default)]
struct State {
    lists: HashMap<String, RecordedList>,
    recorded: HashMap<(String, ModelConnection), Recorded>,
    flights: HashMap<FlightKey, Flight>,
}

pub(crate) struct TeamPlans {
    http: Http,
    origins: AccountOrigins,
    state: Mutex<State>,
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    // 持锁的代码不 panic、不 await；中毒时数据仍然一致，继续使用。
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

impl TeamPlans {
    pub fn new(egress: Arc<Egress>, origins: AccountOrigins) -> Self {
        Self {
            http: Http::new(egress),
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

    /// 这一轮的结论；未连接时没有团队，不联网。
    pub async fn refresh(&self, account: &TeamAccount) -> Round {
        let Some(token) = account.connected() else {
            return Round::default();
        };
        let key = (
            account.provider.clone(),
            account.identity.clone(),
            account.token_print(),
        );
        let flight = lock(&self.state)
            .flights
            .entry(key.clone())
            .or_default()
            .clone();
        flight
            .get_or_init(|| async {
                let round = self.query(account, token).await;
                let mut state = lock(&self.state);
                if state
                    .flights
                    .get(&key)
                    .is_some_and(|f| Arc::ptr_eq(f, &flight))
                {
                    state.flights.remove(&key);
                }
                round
            })
            .await
            .clone()
    }

    async fn query(&self, account: &TeamAccount, token: &str) -> Round {
        let origin = self.origin(account.family);
        let print = account.token_print();
        // 记下的列表：同一 profile id 与 token 时直接用；只有 profile id 相同时，列表读取失败才沿用它。
        let (cached, previous) = {
            let state = lock(&self.state);
            let same = state
                .lists
                .get(&account.provider)
                .filter(|list| list.identity == account.identity);
            let cached = same
                .filter(|list| list.token == print)
                .map(|l| l.plans.clone());
            (cached, same.map(|list| list.plans.clone()))
        };
        let mut networked = false;
        let (fresh, mut customer) = match cached {
            Some(_) => (None, None),
            None => {
                networked = true;
                let (plans, customer) = list::fetch(&self.http, origin, token).await;
                (plans, Some(customer))
            }
        };
        let plans = fresh.clone().or(cached).or(previous).unwrap_or_default();
        let known: Vec<Option<Availability>> = plans
            .iter()
            .map(|plan| self.known(account, &print, &plan.id))
            .collect();
        if known.iter().any(Option::is_none) && customer.is_none() {
            networked = true;
            customer = Some(list::customer_info(&self.http, origin, token).await);
        }
        let checks = plans.iter().zip(&known).map(|(plan, known)| {
            let customer = &customer;
            async move {
                if let Some(availability) = known {
                    return *availability;
                }
                let Some(customer) = customer else {
                    return Availability::Unknown;
                };
                match check::verify(customer, &plan.id) {
                    Ok(organization) => {
                        let input = check::Detail {
                            origin,
                            family: account.family,
                            team: &plan.id,
                            token,
                            organization: &organization,
                        };
                        check::detail(&self.http, input).await
                    }
                    Err(availability) => availability,
                }
            }
        });
        let results = join_all(checks).await;
        tracing::info!(
            family = account.family.as_str(),
            listed = fresh.as_ref().map(Vec::len),
            results = ?results,
            "team plans"
        );
        Round {
            fresh,
            plans: plans.into_iter().zip(results).collect(),
            networked,
        }
    }

    /// 记下的确定结论（同一 profile id 与 token）。
    fn known(
        &self,
        account: &TeamAccount,
        print: &str,
        id: &ModelConnection,
    ) -> Option<Availability> {
        let state = lock(&self.state);
        let recorded = state
            .recorded
            .get(&(account.provider.clone(), id.clone()))?;
        (recorded.identity == account.identity
            && recorded.token == print
            && recorded.availability != Availability::Unknown)
            .then_some(recorded.availability)
    }

    /// 发布前记下这一轮，给出 entitled 的团队项目（按列表顺序）：available true、unavailable false、unknown 沿用上次
    /// 同范围的值（第一次 false；换了 profile id 不沿用，App `resetPrevious`）。
    pub fn commit(&self, account: &TeamAccount, round: Round) -> Vec<TeamPlan> {
        let print = account.token_print();
        let mut state = lock(&self.state);
        if let Some(plans) = round.fresh {
            state.lists.insert(
                account.provider.clone(),
                RecordedList {
                    identity: account.identity.clone(),
                    token: print.clone(),
                    plans,
                },
            );
        }
        let mut entitled = Vec::new();
        for (plan, availability) in round.plans {
            let key = (account.provider.clone(), plan.id.clone());
            let previous = state
                .recorded
                .get(&key)
                .filter(|recorded| recorded.identity == account.identity)
                .is_some_and(|recorded| recorded.entitled);
            let yes = match availability {
                Availability::Available => true,
                Availability::Unavailable(_) => false,
                Availability::Unknown => previous,
            };
            state.recorded.insert(
                key,
                Recorded {
                    identity: account.identity.clone(),
                    token: print.clone(),
                    availability,
                    entitled: yes,
                },
            );
            if yes {
                entitled.push(plan);
            }
        }
        entitled
    }

    /// 记下的已过期 / 未分配（只这两种有 App 的文案）：给了团队项目时只看它，否则按列表顺序第一个。
    pub fn notice(&self, account: &TeamAccount, team: Option<&ModelConnection>) -> Option<Reason> {
        account.connected()?;
        let state = lock(&self.state);
        let reason = |id: &ModelConnection| {
            let recorded = state
                .recorded
                .get(&(account.provider.clone(), id.clone()))?;
            match recorded.availability {
                Availability::Unavailable(reason @ (Reason::Expired | Reason::Unassigned))
                    if recorded.identity == account.identity =>
                {
                    Some(reason)
                }
                _ => None,
            }
        };
        match team {
            Some(team) => reason(team),
            None => state
                .lists
                .get(&account.provider)?
                .plans
                .iter()
                .find_map(|plan| reason(&plan.id)),
        }
    }

    /// 管理团队套餐的地址（App `MODEL_PROVIDER_FAMILY_SPECS` 的 `teamCodingPlanManageUrl`）。z.ai 写死，App 也没有测试
    /// 环境的变体（F58 核对过：F55 以为是 bug）；BigModel 只看 `ZCODE_ENV`（入口算好，`AccountOrigins::bigmodel_team_plans`）。
    /// 修复线 F58：BigModel 以前从账号地址拼，设了 `BIGMODEL_*` 覆盖变量时跟着变，App 不跟。
    pub fn manage_url(&self, family: Family) -> String {
        match family {
            Family::Zai => "https://z.ai/manage-apikey/subscription".to_owned(),
            Family::Bigmodel => self.origins.bigmodel_team_plans.clone(),
        }
    }
}

/// 已过期 / 未分配的提示（App i18n 的英文，后接管理链接，platform.md §9.2）。
pub(crate) fn notice_text(reason: Reason, manage_url: &str) -> Option<String> {
    let text = match reason {
        Reason::Expired => "Team plan expired. Contact your team administrator.",
        Reason::Unassigned => "Team plan not assigned. Contact your team admin.",
        Reason::AuthFailed | Reason::NotEntitled => return None,
    };
    Some(format!("{text} Manage team plan: {manage_url}"))
}

#[cfg(test)]
#[path = "check_tests.rs"]
mod check_tests;

#[cfg(test)]
#[path = "platform_tests.rs"]
mod platform_tests;
