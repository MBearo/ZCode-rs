//! Team Plan 经平台层（platform.md §9.1–§9.7，工作线 F49）：团队列表 → 每个项目的权益 → 目录平铺（带团队项目）→
//! 每请求鉴权 → 团队项目令牌，对本地假服务与临时 HOME。App 的连接选择（`setting.json`）不影响任何结果。

use rust_cli_base::models;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use rust_cli_kernel_api::model::{ModelConnection, ModelError, ModelSelection};
use rust_cli_kernel_api::provider::{
    CatalogModel, Credential, ModelDirectory, ModelEndpoint, ModelEndpoints,
};
use serde_json::{Value, json};

use crate::fake_server_tests::{Captured, Gate, Harness, NOW_MS, Reply, Server};
use crate::test_support::write;

const TEAM: &str = "account:bigmodel-team-coding-plan";
const PERSONAL: &str = "account:bigmodel-individual-coding-plan";
const DETAIL: &str = "/api/biz/team/subscribe/product/querySubscribeDetail";
const PRICING: &str = "/api/biz/subscription/enterprise/v2/pricing";
const CUSTOMER: &str = "/api/biz/customer/getCustomerInfo";
const SUBSCRIPTION: &str = "/api/biz/subscription/list";

/// 假账号服务：pricing 里一个只给了组织的已订阅产品；customerInfo 有个人项目与 org-t 下的团队项目 p-t、p-u；订阅
/// 详情按项目取 `details` 里的回复（列表的最后一个一直用），没有时 VALID；个人订阅有效（工作线 F50）。每个请求记下
/// 到达的时刻。
struct Service {
    details: Mutex<HashMap<String, Vec<Reply>>>,
    pricing: Mutex<Vec<Reply>>,
    issued: AtomicUsize,
    arrived: Mutex<Vec<(String, Instant)>>,
    delay: Mutex<HashMap<String, u64>>,
}

fn ok(data: Value) -> Reply {
    Reply::json(200, &json!({"code": 200, "data": data}))
}

fn detail(status: &str, grant: &str) -> Reply {
    ok(json!({"hasSubscription": true, "status": status, "memberGrantStatus": grant}))
}

fn pricing() -> Reply {
    ok(json!({"productList": [
        {"productId": "prod-1", "tier": "PRO", "subscribeMode": "CONTINUOUS", "subscribePeriod": "MONTHLY",
         "subscribed": true, "organizationId": "org-t"},
        {"productId": "prod-2", "tier": "LITE", "subscribed": false}
    ]}))
}

fn next(replies: &mut Vec<Reply>) -> Option<Reply> {
    if replies.len() > 1 {
        return Some(replies.remove(0));
    }
    let only = replies.first()?;
    Some(Reply::json(
        only.status,
        &serde_json::from_str::<Value>(&only.body).unwrap(),
    ))
}

impl Service {
    fn new(details: &[(&str, Vec<Reply>)]) -> Arc<Self> {
        Arc::new(Self {
            details: Mutex::new(
                details
                    .iter()
                    .map(|(project, replies)| {
                        let replies = replies
                            .iter()
                            .map(|r| Reply::json(r.status, &serde_json::from_str(&r.body).unwrap()))
                            .collect();
                        ((*project).to_owned(), replies)
                    })
                    .collect(),
            ),
            pricing: Mutex::new(vec![pricing()]),
            issued: AtomicUsize::new(0),
            arrived: Mutex::new(Vec::new()),
            delay: Mutex::new(HashMap::new()),
        })
    }

    fn reply(&self, request: &Captured) -> Reply {
        let path = request.path.as_str();
        self.arrived
            .lock()
            .unwrap()
            .push((path.to_owned(), Instant::now()));
        let reply = self.route(request);
        match self.delay.lock().unwrap().get(path) {
            Some(millis) => reply.after(*millis),
            None => reply,
        }
    }

    fn route(&self, request: &Captured) -> Reply {
        let path = request.path.as_str();
        if path == PRICING {
            return next(&mut self.pricing.lock().unwrap()).unwrap();
        }
        if path == DETAIL {
            let project = request.header("bigmodel-project").unwrap_or_default();
            let mut details = self.details.lock().unwrap();
            return details
                .get_mut(project)
                .and_then(next)
                .unwrap_or_else(|| detail("EFFECTIVE", "VALID"));
        }
        match (request.method.as_str(), path) {
            ("GET", SUBSCRIPTION) => ok(json!([
                {"productName": "GLM Coding Plan", "status": "VALID", "inCurrentPeriod": true}
            ])),
            ("GET", CUSTOMER) => ok(json!({"organizations": [
                {"organizationId": "org-1", "organizationName": "默认机构", "projects": [
                    {"projectId": "p-own", "projectName": "默认项目", "projectType": 1}
                ]},
                {"organizationId": "org-t", "organizationName": "团队", "projects": [
                    {"projectId": "p-t", "projectType": 2},
                    {"projectId": "p-u", "projectType": 2}
                ]}
            ]})),
            ("GET", p) if p.ends_with("/api_keys") => {
                let team = p.contains("org-t");
                let id = ["p-t", "p-u"]
                    .into_iter()
                    .find(|project| p.contains(project))
                    .map_or("key-own".to_owned(), |project| format!("key-{project}"));
                let name = if team {
                    "zcode-team-api-key"
                } else {
                    "zcode-api-key"
                };
                ok(json!([{"name": name, "apiKey": id, "keyType": if team { 2 } else { 1 }}]))
            }
            ("POST", p) if p.ends_with("/access_tokens") => {
                let n = self.issued.fetch_add(1, Ordering::SeqCst) + 1;
                let now = NOW_MS / 1000;
                ok(json!({
                    "accessToken": format!("pat-{n}"),
                    "tokenType": "Bearer",
                    "expiresIn": 3600,
                    "expiresAt": now + 3600,
                }))
            }
            _ => Reply::json(404, &json!({"code": 404})),
        }
    }

    fn delay(&self, path: &str, millis: u64) {
        self.delay.lock().unwrap().insert(path.to_owned(), millis);
    }

    /// 两个路径第一次到达的间隔（毫秒）。
    fn gap(&self, a: &str, b: &str) -> u128 {
        let arrived = self.arrived.lock().unwrap();
        let at = |path: &str| arrived.iter().find(|(p, _)| p == path).unwrap().1;
        at(b)
            .max(at(a))
            .duration_since(at(a).min(at(b)))
            .as_millis()
    }
}

async fn start(service: Arc<Service>) -> (Server, Harness) {
    let server = Server::start({
        let service = service.clone();
        move |request| service.reply(request)
    })
    .await;
    let harness = Harness::new(&server.url()).await;
    (server, harness)
}

/// bigmodel 的 OAuth 登录（App 写的键；个人套餐照 App 按 profile id 与个人订阅授权，工作线 F50）。
async fn login(harness: &Harness, token: &str) {
    harness
        .write_credentials(&json!({
            "oauth:bigmodel:access_token": token,
            "oauth:bigmodel:user_info": r#"{"id":"key-1","username":"dev","displayName":"Dev"}"#,
            "zcodejwttoken": "zcode-jwt",
        }))
        .await;
}

fn team(project: &str) -> ModelConnection {
    ModelConnection {
        product_id: "prod-1".to_owned(),
        organization_id: "org-t".to_owned(),
        project_id: project.to_owned(),
    }
}

fn model(provider: &str, project: Option<&str>) -> ModelSelection {
    ModelSelection {
        provider: provider.to_owned(),
        model: models::GLM_5_3_ACCOUNT.to_owned(),
        reasoning: None,
        connection: project.map(|project| Box::new(team(project))),
    }
}

/// 目录里账号模型 GLM-5.3 的每一项：(provider, 团队项目)。
async fn rows(harness: &Harness) -> Vec<(String, Option<String>)> {
    let models: Vec<CatalogModel> = harness.platform.catalog().await.models;
    models
        .into_iter()
        .filter(|entry| {
            entry.provider.starts_with("account:") && entry.model == models::GLM_5_3_ACCOUNT
        })
        .map(|entry| {
            let project = entry.connection.map(|c| c.id.project_id);
            (entry.provider, project)
        })
        .collect()
}

fn row(provider: &str, project: Option<&str>) -> (String, Option<String>) {
    (provider.to_owned(), project.map(str::to_owned))
}

fn token(endpoint: &ModelEndpoint) -> &str {
    match &endpoint.credential {
        Credential::ApiKey(key) => key.expose(),
        Credential::None => "",
    }
}

#[tokio::test]
async fn every_usable_plan_is_listed_flat() {
    let service = Service::new(&[("p-u", vec![detail("EFFECTIVE", "UNASSIGNED")])]);
    // pricing 与 customerInfo 同时发，两个项目的订阅详情也同时发（各停 800 毫秒，顺序发时间隔不会小于它）。
    for path in [PRICING, CUSTOMER, DETAIL] {
        service.delay(path, 800);
    }
    let (server, harness) = start(service.clone()).await;
    login(&harness, "at-big").await;
    // 个人与团队同时出现；未分配的团队项目不出现。
    assert_eq!(
        rows(&harness).await,
        [row(PERSONAL, None), row(TEAM, Some("p-t"))]
    );
    assert!(service.gap(PRICING, CUSTOMER) < 800);
    let arrived = service.arrived.lock().unwrap().clone();
    let details: Vec<Instant> = arrived
        .iter()
        .filter(|(path, _)| path == DETAIL)
        .map(|(_, at)| *at)
        .collect();
    assert_eq!(details.len(), 2);
    assert!(details[1].duration_since(details[0]).as_millis() < 800);
    // 团队的 customerInfo 只取一次（列表与两个项目的核对共用，D-F49-1）；个人项目令牌的位置另取一次（App 相同）。
    // 团队头与鉴权照 App。
    assert_eq!(server.count(CUSTOMER), 2);
    assert_eq!(server.count(PRICING), 1);
    let requests = server.requests();
    let pricing = requests.iter().find(|r| r.path == PRICING).unwrap();
    assert_eq!(pricing.header("authorization"), Some("at-big"));
    let checked: Vec<_> = requests
        .iter()
        .filter(|r| r.path == DETAIL)
        .map(|r| {
            (
                r.header("authorization").unwrap().to_owned(),
                r.header("bigmodel-organization").unwrap().to_owned(),
            )
        })
        .collect();
    let expected = ("at-big".to_owned(), "org-t".to_owned());
    assert_eq!(checked, [expected.clone(), expected]);
    // 目录项的说明材料：供应商名与团队显示名（App `formatTeamPlanDisplayName`：组织名）。
    let catalog = harness.platform.catalog().await;
    let entry = catalog
        .models
        .iter()
        .find(|entry| entry.provider == TEAM)
        .unwrap();
    let connection = entry.connection.as_ref().unwrap();
    assert_eq!(
        (connection.vendor.as_str(), connection.name.as_str()),
        ("BigModel", "团队")
    );
    assert_eq!(connection.id, team("p-t"));
    // 结果记下了：再取目录不再联网。团队 4 个请求（pricing、customerInfo、两个订阅详情），个人 4 个（customerInfo、
    // key 列表、签发、个人订阅）。
    assert_eq!(server.count("/api/biz/"), 8);
    harness.advance(1);
    rows(&harness).await;
    assert_eq!(server.count("/api/biz/"), 8);
}

#[tokio::test]
async fn the_individual_plan_and_the_team_plans_are_queried_together() {
    let service = Service::new(&[]);
    // pricing 与个人订阅各停 800 毫秒：先后查时，个人订阅要等团队查完才发出，与 pricing 到达的间隔不会小于它。
    service.delay(PRICING, 800);
    service.delay(SUBSCRIPTION, 800);
    let (_server, harness) = start(service.clone()).await;
    login(&harness, "at-big").await;
    let started = Instant::now();
    assert_eq!(rows(&harness).await.len(), 3);
    assert!(service.gap(PRICING, SUBSCRIPTION) < 800);
    assert!(started.elapsed().as_millis() < 1_600);
}

#[tokio::test]
async fn the_app_connection_does_not_matter() {
    let (server, harness) = start(Service::new(&[])).await;
    login(&harness, "at-big").await;
    // App 选了个人 / 另一个团队：本工程不读，目录与请求都不变。
    for selection in [
        json!({"bigmodel": {"kind": "individual-coding-plan"}}),
        json!({"bigmodel": {"kind": "team-coding-plan", "productId": "prod-9", "organizationId": "org-t", "projectId": "p-u"}}),
    ] {
        let settings = json!({"providerFamilyDomain": "bigmodel", "providerFamilyConnectionSelections": selection});
        write(harness.paths.settings().as_path(), settings.to_string()).await;
        assert_eq!(
            rows(&harness).await,
            [
                row(PERSONAL, None),
                row(TEAM, Some("p-t")),
                row(TEAM, Some("p-u"))
            ]
        );
    }
    // 选哪一项就用哪个套餐的令牌：团队项目 p-t 的 key（团队头）、个人的 key。
    let team_endpoint = harness
        .platform
        .endpoint(&model(TEAM, Some("p-t")), None)
        .await
        .unwrap();
    let personal_endpoint = harness
        .platform
        .endpoint(&model(PERSONAL, None), None)
        .await
        .unwrap();
    assert_ne!(token(&team_endpoint), token(&personal_endpoint));
    let issued: Vec<String> = server
        .requests()
        .into_iter()
        .filter(|r| r.path.ends_with("/access_tokens"))
        .map(|r| r.path)
        .collect();
    // 个人的令牌在取目录时为订阅查询换好（工作线 F50），请求直接用，不再签发。
    assert_eq!(
        issued,
        [
            "/api/biz/v1/organization/org-1/projects/p-own/api_keys/key-own/access_tokens",
            "/api/biz/v1/organization/org-t/projects/p-t/api_keys/key-p-t/access_tokens",
        ]
    );
    let key_list = server
        .requests()
        .into_iter()
        .find(|r| r.path == "/api/biz/v1/organization/org-t/projects/p-t/api_keys")
        .unwrap();
    assert_eq!(key_list.header("authorization"), Some("at-big"));
    assert_eq!(key_list.header("bigmodel-project"), Some("p-t"));
    // 选择没带团队项目（Node / App 写的会话）：取这个模型的第一个团队项目。
    let first = harness
        .platform
        .endpoint(&model(TEAM, None), None)
        .await
        .unwrap();
    assert_eq!(token(&first), token(&team_endpoint));
}

#[tokio::test]
async fn two_team_projects_request_concurrently() {
    let service = Service::new(&[]);
    let (server, harness) = start(service.clone()).await;
    let harness = Arc::new(harness);
    login(&harness, "at-big").await;
    assert_eq!(rows(&harness).await.len(), 3);
    service.delay("/api/biz/v1/organization/org-t/projects/p-t/api_keys", 300);
    let (first, second) = (model(TEAM, Some("p-t")), model(TEAM, Some("p-u")));
    let (a, b) = tokio::join!(
        harness.platform.endpoint(&first, None),
        harness.platform.endpoint(&second, None),
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_ne!(token(&a), token(&b));
    let mut issued: Vec<String> = server
        .requests()
        .into_iter()
        .filter(|r| r.path.ends_with("/access_tokens") && r.path.contains("/org-t/"))
        .map(|r| r.path)
        .collect();
    issued.sort();
    assert_eq!(
        issued,
        [
            "/api/biz/v1/organization/org-t/projects/p-t/api_keys/key-p-t/access_tokens",
            "/api/biz/v1/organization/org-t/projects/p-u/api_keys/key-p-u/access_tokens",
        ]
    );
    // 各自的账号范围：p-t 的令牌被拒时不拿 p-u 的范围重发；换了登录 token 之后旧令牌的 401 不重发。
    login(&harness, "at-big-2").await;
    let message = match harness
        .platform
        .endpoint(&model(TEAM, Some("p-t")), Some(&a))
        .await
    {
        Err(ModelError::Authentication { message }) => message,
        other => panic!("expected scope invalidation, got {other:?}"),
    };
    assert!(
        message.starts_with("project_token_scope_invalidated"),
        "{message}"
    );
}

#[tokio::test]
async fn unknown_keeps_the_last_result_and_expiry_explains_itself() {
    let service = Service::new(&[(
        "p-t",
        vec![
            Reply::json(500, &json!({})),
            detail("EFFECTIVE", "VALID"),
            Reply::json(503, &json!({})),
            detail("EXPIRED", "VALID"),
        ],
    )]);
    let (server, harness) = start(service.clone()).await;
    login(&harness, "at-1").await;
    // 第一次 unknown：沿用上次，第一次是 false。p-u 可用。
    assert_eq!(
        rows(&harness).await,
        [row(PERSONAL, None), row(TEAM, Some("p-u"))]
    );
    // unknown 不记：下一次重建（这里同样的凭据重写一次，文件的元数据变了）再查，这次可用。
    login(&harness, "at-1").await;
    assert_eq!(
        rows(&harness).await,
        [
            row(PERSONAL, None),
            row(TEAM, Some("p-t")),
            row(TEAM, Some("p-u"))
        ]
    );
    // 重新登录换了 token、范围不变：列表与权益重查；这次 unknown，沿用上次的“可用”。
    login(&harness, "at-2").await;
    assert_eq!(rows(&harness).await.len(), 3);
    assert_eq!(server.count(PRICING), 2);
    assert_eq!(harness.platform.team_notice(None).await, None);
    // 又换 token：这次已过期，不出现，提示照 App。
    login(&harness, "at-3").await;
    assert_eq!(
        rows(&harness).await,
        [row(PERSONAL, None), row(TEAM, Some("p-u"))]
    );
    let manage = format!("{}/coding-plan/team/plans", server.url());
    let expected =
        format!("Team plan expired. Contact your team administrator. Manage team plan: {manage}");
    assert_eq!(
        harness.platform.team_notice(Some(TEAM)).await.as_deref(),
        Some(expected.as_str())
    );
    assert_eq!(harness.platform.team_notice(Some(PERSONAL)).await, None);
    // 过期的团队项目取端点：报 App 的文案；可用的另一个项目照常。
    let message = match harness
        .platform
        .endpoint(&model(TEAM, Some("p-t")), None)
        .await
    {
        Err(ModelError::Authentication { message }) => message,
        other => panic!("expected the team notice, got {other:?}"),
    };
    assert_eq!(message, expected);
    assert!(
        harness
            .platform
            .endpoint(&model(TEAM, Some("p-u")), None)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn a_list_failure_keeps_the_last_list() {
    let service = Service::new(&[]);
    let (server, harness) = start(service.clone()).await;
    login(&harness, "at-1").await;
    assert_eq!(rows(&harness).await.len(), 3);
    // 换 token 后 pricing 失败：沿用同一 profile id 的上次列表（权益照常按新 token 查）。
    *service.pricing.lock().unwrap() = vec![Reply::json(500, &json!({}))];
    login(&harness, "at-2").await;
    assert_eq!(rows(&harness).await.len(), 3);
    assert_eq!(server.count(PRICING), 2);
}

#[tokio::test]
async fn an_account_change_during_the_query_discards_the_round() {
    // 登录之后的第一个按旧 token 的 detail 请求停在闸门上，换完账号再放行（F123：以前固定停 300 毫秒）。
    let arrived = Arc::new(tokio::sync::Notify::new());
    let (gate, armed) = (Gate::default(), Arc::new(AtomicBool::new(false)));
    let service = Service::new(&[]);
    let server = Server::start({
        let (service, arrived, gate, armed) = (
            service.clone(),
            arrived.clone(),
            gate.clone(),
            armed.clone(),
        );
        move |request| {
            let reply = service.reply(request);
            if request.path == DETAIL
                && request.header("authorization") == Some("at-1")
                && armed.swap(false, Ordering::SeqCst)
            {
                arrived.notify_one();
                reply.until(&gate)
            } else {
                reply
            }
        }
    })
    .await;
    let harness = Arc::new(Harness::new(&server.url()).await);
    login(&harness, "at-1").await;
    armed.store(true, Ordering::SeqCst);
    let pending = tokio::spawn({
        let harness = harness.clone();
        async move { harness.platform.catalog().await }
    });
    arrived.notified().await;
    login(&harness, "at-2").await;
    gate.open();
    // 这一轮丢弃：没有发布任何快照。
    let first = pending.await.unwrap();
    assert_eq!(first.revision, 0);
    assert!(first.models.is_empty());
    // 下一次按新 token 查。
    assert_eq!(rows(&harness).await.len(), 3);
    let tokens: Vec<String> = server
        .requests()
        .iter()
        .filter(|r| r.path == PRICING)
        .filter_map(|r| r.header("authorization").map(str::to_owned))
        .collect();
    assert_eq!(tokens, ["at-1", "at-2"]);
}

#[tokio::test]
async fn a_login_change_during_the_exchange_invalidates_the_token() {
    // 布置之后的第一个换取请求停在闸门上，换完登录再放行（F123：以前固定停 300 毫秒）。
    let arrived = Arc::new(tokio::sync::Notify::new());
    let delay = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let gate = Gate::default();
    let service = Service::new(&[]);
    let server = Server::start({
        let (service, arrived, delay, gate) = (
            service.clone(),
            arrived.clone(),
            delay.clone(),
            gate.clone(),
        );
        move |request| {
            let reply = service.reply(request);
            if request.path.ends_with("/access_tokens") && delay.swap(false, Ordering::SeqCst) {
                arrived.notify_one();
                reply.until(&gate)
            } else {
                reply
            }
        }
    })
    .await;
    let harness = Arc::new(Harness::new(&server.url()).await);
    login(&harness, "at-big").await;
    assert_eq!(rows(&harness).await.len(), 3);
    delay.store(true, Ordering::SeqCst);
    let pending = tokio::spawn({
        let harness = harness.clone();
        async move {
            harness
                .platform
                .endpoint(&model(TEAM, Some("p-t")), None)
                .await
        }
    });
    arrived.notified().await;
    login(&harness, "at-big-2").await;
    gate.open();
    let message = match pending.await.unwrap() {
        Err(ModelError::Authentication { message }) => message,
        other => panic!("expected scope invalidation, got {other:?}"),
    };
    assert!(
        message.starts_with("project_token_scope_invalidated"),
        "{message}"
    );
    // 直接取团队凭据、没带团队项目：App 的原文。
    let error = harness
        .platform
        .auth
        .credential((TEAM, "bigmodel", "team-coding-plan"), None, None)
        .await;
    assert!(
        matches!(&error, Err(ModelError::Authentication { message })
            if message == "Account request credential is unavailable: account:bigmodel-team-coding-plan"),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_stale_zcode_jwt_is_not_a_bigmodel_login() {
    let (server, harness) = start(Service::new(&[])).await;
    // 旧版本误把 zcodejwttoken 写进 bigmodel 的 access token：团队未连接，不查、不授权（App 相同）。个人套餐照 App
    // 没有这条防御，照常换令牌、查订阅（§5.7）。
    login(&harness, "zcode-jwt").await;
    assert!(
        rows(&harness)
            .await
            .iter()
            .all(|(provider, _)| provider != TEAM)
    );
    assert_eq!(server.count(PRICING), 0);
    assert_eq!(server.count(DETAIL), 0);
}
