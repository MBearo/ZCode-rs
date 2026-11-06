//! `/usage` 的端口（platform.md §11.1、§11.2）：计划清单与 id、一分钟的缓存与同一问题只问一次、额度池与历史的整理
//! （对照 usageHost.test.ts 的口径）。

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use rust_cli_tui::services::PlanUsage;

use rust_cli_kernel_api::model::ModelConnection;
use rust_cli_kernel_api::provider::CatalogConnection;
use rust_cli_platform::usage::{
    ActivitySnapshot, FamilyPlans, HealthSnapshot, IndividualProvider, ModelUsage, ProviderFacts,
    Quota, TeamProvider, ToolUsage, UsageDetail,
};
use tokio::sync::Notify;

use super::*;

const ZAI: &str = "account:zai-individual-coding-plan";
const ZAI_TEAM: &str = "account:zai-team-coding-plan";
const BIGMODEL: &str = "account:bigmodel-individual-coding-plan";
const BIGMODEL_TEAM: &str = "account:bigmodel-team-coding-plan";

struct Clock(AtomicU64);

impl rust_cli_kernel_api::clock::Clock for Clock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
    fn time_zone(&self) -> String {
        "UTC".into()
    }
}

fn team(project: &str, name: &str) -> CatalogConnection {
    CatalogConnection {
        id: ModelConnection {
            product_id: "prod".into(),
            organization_id: "org".into(),
            project_id: project.into(),
        },
        vendor: "BigModel".into(),
        name: name.into(),
    }
}

/// zai 只有个人（已授权），bigmodel 个人没有授权、有两个团队项目。
fn view() -> UsageView {
    UsageView {
        families: vec![
            FamilyPlans {
                family: AccountFamily::Zai,
                individual: Some(IndividualProvider {
                    provider: ZAI.into(),
                    entitled: true,
                }),
                team: Some(TeamProvider {
                    provider: ZAI_TEAM.into(),
                    teams: Vec::new(),
                }),
            },
            FamilyPlans {
                family: AccountFamily::Bigmodel,
                individual: Some(IndividualProvider {
                    provider: BIGMODEL.into(),
                    entitled: false,
                }),
                team: Some(TeamProvider {
                    provider: BIGMODEL_TEAM.into(),
                    teams: vec![team("p-1", "Acme"), team("p-2", "Beta")],
                }),
            },
        ],
        providers: vec![
            ProviderFacts {
                provider: "personal:mine".into(),
                name: " Mine ".into(),
                key_url: Some("https://z.ai:8443/manage-apikey/apikey-list".into()),
            },
            ProviderFacts {
                provider: BIGMODEL_TEAM.into(),
                name: "BigModel Team Coding Plan".into(),
                key_url: None,
            },
        ],
    }
}

/// 替身：记下每次问的目标；`gate` 有时额度的请求等它放行；`fail` 为真时失败。
#[derive(Default)]
struct Fake {
    quotas: AtomicUsize,
    histories: AtomicUsize,
    targets: Mutex<Vec<PlanTarget>>,
    gate: Mutex<Option<Arc<Notify>>>,
    fail: std::sync::atomic::AtomicBool,
}

impl PlanSource for Fake {
    fn view(&self) -> BoxFuture<'_, UsageView> {
        async { view() }.boxed()
    }

    fn quota(&self, target: PlanTarget) -> BoxFuture<'_, Result<QuotaSnapshot, String>> {
        async move {
            self.quotas.fetch_add(1, Ordering::SeqCst);
            self.targets.lock_unpoisoned().push(target);
            let gate = self.gate.lock_unpoisoned().clone();
            if let Some(gate) = gate {
                gate.notified().await;
            }
            if self.fail.load(Ordering::SeqCst) {
                return Err("quota failed".into());
            }
            Ok(snapshot(PlanStatus::Available))
        }
        .boxed()
    }

    fn history(
        &self,
        target: PlanTarget,
        _range: HistoryRange,
    ) -> BoxFuture<'_, Result<HistorySnapshot, String>> {
        async move {
            self.histories.fetch_add(1, Ordering::SeqCst);
            self.targets.lock_unpoisoned().push(target);
            Ok(history())
        }
        .boxed()
    }
}

fn limit(kind: &str, unit: f64, number: f64, percentage: f64) -> QuotaLimit {
    QuotaLimit {
        kind: kind.into(),
        unit: Some(unit),
        number: Some(number),
        percentage: Some(percentage),
        ..QuotaLimit::default()
    }
}

fn snapshot(plan: PlanStatus) -> QuotaSnapshot {
    QuotaSnapshot {
        at_ms: 42,
        plan,
        quota: Some(Quota {
            level: Some(" pro".into()),
            limits: vec![
                limit("TIME_LIMIT", 5.0, 1.0, 12.0),
                QuotaLimit {
                    next_reset_ms: Some(1_700_000_000_000.0),
                    ..limit("CREDIT_LIMIT", 3.0, 5.0, 130.0)
                },
                limit("TOKENS_LIMIT", 6.0, 7.0, 40.5),
                limit("TOKENS_LIMIT", 3.0, 4.0, 1.0),
            ]
            .into_iter()
            .map(|mut limit| {
                if limit.kind == "TIME_LIMIT" {
                    limit.remaining = Some(88.0);
                    limit.details = vec![UsageDetail {
                        code: "search".into(),
                        name: None,
                        usage: 7.4,
                    }];
                }
                limit
            })
            .collect(),
        }),
        mcp: Some(limit("MCP_USAGE", 0.0, 0.0, 25.0)),
        renews_ms: Some(7),
        ends_ms: None,
    }
}

fn history() -> HistorySnapshot {
    HistorySnapshot {
        at_ms: 9,
        hourly: false,
        axis: vec!["2026-10-29".into(), "2026-10-30".into()],
        models: vec![
            ModelUsage {
                name: "glm-air".into(),
                cached: vec![1.0, 2.0],
                uncached: vec![3.0, 4.0],
                output: vec![5.0, 6.0],
                total_tokens: 21.0,
            },
            ModelUsage {
                name: "GLM-5".into(),
                cached: vec![10.0],
                uncached: vec![0.0, 0.0],
                output: vec![0.0, 0.0],
                total_tokens: 100.0,
            },
            ModelUsage {
                name: "idle".into(),
                ..ModelUsage::default()
            },
        ],
        total_tokens: 121.0,
        cache_hit_rate: Some(42.0),
        total_credits: 1.5,
        tools: vec![
            ToolUsage {
                name: "search".into(),
                calls: 3.0,
            },
            ToolUsage {
                name: "reader".into(),
                calls: 3.0,
            },
        ],
        activity: ActivitySnapshot {
            total_tokens: 900.0,
            peak_daily_tokens: 400.0,
            streak_days: 2.0,
            start: Some("2026-10-18".into()),
            weeks: vec![[None, None, Some(1), None, None, None, Some(4)]],
        },
        health: HealthSnapshot {
            axis: vec!["2026-10-30".into()],
            pro_max: vec![80.0],
            lite: vec![60.0],
        },
    }
}

fn port(fake: &Arc<Fake>, clock: &Arc<Clock>) -> UsagePort {
    UsagePort::new(fake.clone(), clock.clone())
}

fn rig() -> (Arc<Fake>, Arc<Clock>, UsagePort) {
    let fake = Arc::new(Fake::default());
    let clock = Arc::new(Clock(AtomicU64::new(1_000)));
    let port = port(&fake, &clock);
    (fake, clock, port)
}

async fn plans(port: &UsagePort, provider: &str) -> (UsageModel, Vec<String>) {
    let (model, plans) = port.plans(provider.into()).await.unwrap();
    (model, plans.into_iter().map(|p| p.id).collect())
}

/// 清单：zai 个人（已授权）、bigmodel 的两个团队项目；bigmodel 个人没有授权，只在它是当前模型时列出。当前模型的计划在最前；
/// 团队 provider 是它的第一个团队项目。
#[tokio::test]
async fn plans_follow_the_app_order() {
    let (_, _, port) = rig();
    let acme = "team:bigmodel:prod:org:p-1";
    let beta = "team:bigmodel:prod:org:p-2";
    let (model, ids) = plans(&port, "personal:mine").await;
    assert_eq!(ids, [ZAI, acme, beta]);
    assert_eq!(
        model,
        UsageModel {
            provider_id: "personal:mine".into(),
            official: false,
            name: "Mine".into(),
            site: "z.ai:8443".into(),
        }
    );
    let (_, ids) = plans(&port, BIGMODEL).await;
    assert_eq!(ids, [BIGMODEL, ZAI, acme, beta]);
    let (model, ids) = plans(&port, BIGMODEL_TEAM).await;
    assert_eq!(ids, [acme, ZAI, beta]);
    assert!(model.official);
    assert_eq!(model.site, "");
    let (model, _) = plans(&port, "personal:gone").await;
    assert_eq!(model.name, "personal:gone");
    let (_, plans) = port.plans(BIGMODEL_TEAM.into()).await.unwrap();
    assert_eq!(plans[0].name, "Acme");
    assert_eq!(plans[0].kind, PlanKind::Team);
    assert_eq!(plans[0].family, Family::Bigmodel);
}

/// 计划 id 解回目标：团队带上项目；没有授权的个人 provider 也照解；其余是 unknown plan。
#[tokio::test]
async fn plan_ids_resolve_to_targets() {
    let (fake, _, port) = rig();
    port.quota("team:bigmodel:prod:org:p-2".into())
        .await
        .unwrap();
    port.detail(BIGMODEL.into(), UsageRange::Week)
        .await
        .unwrap();
    let targets = fake.targets.lock_unpoisoned().clone();
    assert_eq!(targets[0].provider, BIGMODEL_TEAM);
    assert_eq!(targets[0].team.as_ref().unwrap().project_id, "p-2");
    assert_eq!(
        targets[1],
        PlanTarget {
            provider: BIGMODEL.into(),
            family: AccountFamily::Bigmodel,
            team: None
        }
    );
    assert_eq!(
        port.quota("team:zai:x:y:z".into()).await.unwrap_err(),
        "unknown plan: team:zai:x:y:z"
    );
}

/// 一分钟内同一个问题给同一个结果；同时问只问一次；失败不留；不同的范围各问各的。
#[tokio::test]
async fn answers_are_kept_for_a_minute_and_shared_while_in_flight() {
    let (fake, clock, port) = rig();
    let gate = Arc::new(Notify::new());
    *fake.gate.lock_unpoisoned() = Some(gate.clone());
    let ask = || port.quota(ZAI.into());
    let (first, second, _) = tokio::join!(ask(), ask(), async {
        // 两个问题都在等同一个请求时放行。
        while fake.quotas.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        tokio::task::yield_now().await;
        gate.notify_waiters();
    });
    assert_eq!(first.unwrap(), second.unwrap());
    assert_eq!(fake.quotas.load(Ordering::SeqCst), 1);
    *fake.gate.lock_unpoisoned() = None;
    clock.0.fetch_add(USAGE_FRESH_MS - 1, Ordering::SeqCst);
    ask().await.unwrap();
    assert_eq!(fake.quotas.load(Ordering::SeqCst), 1);
    clock.0.fetch_add(1, Ordering::SeqCst);
    fake.fail.store(true, Ordering::SeqCst);
    assert_eq!(ask().await.unwrap_err(), "quota failed");
    assert_eq!(ask().await.unwrap_err(), "quota failed");
    assert_eq!(fake.quotas.load(Ordering::SeqCst), 3);
    for range in [UsageRange::Today, UsageRange::Week, UsageRange::Week] {
        port.detail(ZAI.into(), range).await.unwrap();
    }
    assert_eq!(fake.histories.load(Ordering::SeqCst), 2);
}

/// 额度池（usageHost `codingPools`）：CREDIT_LIMIT 与 TOKENS_LIMIT 同义；5 小时要 unit 3 number 5；每周只看 unit 6；
/// 工具调用带剩下的次数；MCP 在最后。剩余 = 100 − 用掉的，夹在 0–100。不是 ok 时没有池。
#[test]
fn quota_pools_follow_the_app() {
    let quota = quota_of(&snapshot(PlanStatus::Available));
    assert_eq!(quota.state, QuotaState::Ok);
    assert_eq!(quota.level, "Pro");
    assert_eq!(
        (quota.at, quota.renews, quota.ends, quota.since),
        (42, Some(7), None, None)
    );
    let kinds: Vec<(PoolKind, f64)> = quota.pools.iter().map(|p| (p.pool, p.left)).collect();
    assert_eq!(
        kinds,
        [
            (PoolKind::FiveHour, 0.0),
            (PoolKind::Weekly, 59.5),
            (PoolKind::Tools, 88.0),
            (PoolKind::Mcp, 75.0),
        ]
    );
    assert_eq!(quota.pools[0].reset_at, Some(1_700_000_000_000));
    assert_eq!(quota.pools[0].count, None);
    assert_eq!(quota.pools[2].count, Some(88));
    assert_eq!(
        quota.pools[2].parts,
        [PoolPart {
            code: "search".into(),
            name: String::new(),
            used: 7
        }]
    );
    for (status, state) in [
        (PlanStatus::NoPlan, QuotaState::NoPlan),
        (PlanStatus::TeamExpired, QuotaState::TeamEnded),
        (PlanStatus::TeamUnassigned, QuotaState::NotOnTeam),
        (PlanStatus::NotConfigured, QuotaState::Expired),
    ] {
        let quota = quota_of(&snapshot(status));
        assert_eq!(quota.state, state);
        assert!(quota.pools.is_empty());
    }
}

/// 历史（usageHost `detailOf`）：三条序列按模型相加（短的补 0）；缓存命中率大于 1 时除以 100；模型与工具去掉 0、多到少；
/// 活动的空天是 -1。
#[test]
fn details_follow_the_app() {
    let detail = detail_of(&history());
    assert_eq!(detail.granularity, Granularity::Day);
    assert_eq!(detail.cached, [11, 2]);
    assert_eq!(detail.input, [3, 4]);
    assert_eq!(detail.output, [5, 6]);
    assert_eq!(detail.total.tokens, 121);
    assert_eq!(detail.total.cache_rate, Some(0.42));
    assert_eq!(detail.total.credits, 1.5);
    let names = |items: &[NamedCount]| {
        items
            .iter()
            .map(|i| (i.name.clone(), i.count))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names(&detail.models),
        [("GLM-5".into(), 100), ("glm-air".into(), 21)]
    );
    assert_eq!(
        names(&detail.tools),
        [("search".into(), 3), ("reader".into(), 3)]
    );
    assert_eq!(detail.activity.start, "2026-10-18");
    assert_eq!(detail.activity.levels, [vec![-1, -1, 1, -1, -1, -1, 4]]);
    assert_eq!(
        (
            detail.activity.total,
            detail.activity.best_day,
            detail.activity.streak
        ),
        (900, 400, 2)
    );
    assert_eq!(detail.health.pro_max, [80.0]);
    let mut today = history();
    today.hourly = true;
    today.cache_hit_rate = Some(0.5);
    let detail = detail_of(&today);
    assert_eq!(detail.granularity, Granularity::Hour);
    assert_eq!(detail.total.cache_rate, Some(0.5));
}
