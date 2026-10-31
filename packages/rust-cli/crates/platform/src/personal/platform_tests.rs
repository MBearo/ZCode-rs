//! 个人套餐照 App 的登录授权（platform.md §5.2–§5.7，工作线 F50），经平台层对本地假服务与临时 HOME：只有 App 写的
//! 凭据（`app-login-golden.json` 里 App 的轮询登录写出的加密文件原文）时个人套餐出现在目录并能发请求；项目位置的键
//! 与 App 逐字节相同、App 先建的位置直接复用；订阅查询的结论、沿用与丢弃；手工 key 照旧；请求期不写凭据。

use rust_cli_base::models;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::model::ModelError;
use rust_cli_kernel_api::provider::{Credential, ModelDirectory, ModelEndpoint, ModelEndpoints};
use serde_json::{Value, json};

use crate::CredentialSecret;
use crate::fake_server_tests::{Captured, Gate, Harness, NOW_MS, Reply, Server, selection};
use crate::test_support::write;
use crate::token::Family;
use crate::token::location::storage_key;

const SUBSCRIPTION: &str = "/api/biz/subscription/list";
const CUSTOMER: &str = "/api/biz/customer/getCustomerInfo";
const PRICING: &str = "/api/biz/subscription/enterprise/v2/pricing";

/// App 对照夹具（`scripts/rust-cli/app-login-golden.mts` 生成）。
fn golden() -> Value {
    serde_json::from_str(include_str!("../../tests/fixtures/app-login-golden.json")).unwrap()
}

/// 生成 golden 时 App 用的凭据密钥（测试值）。
fn golden_secret() -> CredentialSecret {
    CredentialSecret {
        explicit: Some(RedactedString::new(golden()["secret"].as_str().unwrap())),
        platform: "linux".to_owned(),
        home: "/Users/dev".to_owned(),
        username: "dev".to_owned(),
    }
}

fn provider(family: &str) -> String {
    format!("account:{family}-individual-coding-plan")
}

fn subscribed() -> Reply {
    Reply::json(
        200,
        &json!({"code": 200, "data": [
            {"productName": "GLM Coding Plan Pro", "status": "VALID", "inCurrentPeriod": true}
        ]}),
    )
}

/// 假账号服务：customerInfo 是 golden 的那一份（App 生成 golden 时的回复），key 列表里没有 `zcode-api-key`（要创建），
/// 签发依次给 `pat-1`、`pat-2`……；订阅列表按 `subscriptions` 依次回复（最后一个一直用）；没有团队。
struct App {
    subscriptions: Mutex<Vec<Reply>>,
    issued: AtomicUsize,
    /// 订阅请求到达时通知、并在回复前停一会儿（查询途中换账号的测试）。
    /// 下一个订阅请求：到达时通知测试，回复停在闸门上（F123：以前是固定停顿的毫秒数）。
    hold: Mutex<Option<(Arc<tokio::sync::Notify>, Gate)>>,
}

impl App {
    fn new(subscriptions: Vec<Reply>) -> Arc<Self> {
        Arc::new(Self {
            subscriptions: Mutex::new(subscriptions),
            issued: AtomicUsize::new(0),
            hold: Mutex::new(None),
        })
    }

    fn script(&self, replies: Vec<Reply>) {
        *self.subscriptions.lock().unwrap() = replies;
    }

    fn reply(&self, request: &Captured) -> Reply {
        let ok = |data: Value| Reply::json(200, &json!({"code": 200, "data": data}));
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", CUSTOMER) => ok(golden()["customer"].clone()),
            // 团队列表（App 的登录也连上了团队，§9.1）：没有团队。列出成功就记下，之后不再查。
            ("GET", PRICING) => ok(json!({"productList": []})),
            ("GET", "/api/biz/v1/organization/org-1/projects/p-1/api_keys") => {
                ok(json!([{"name": "other", "apiKey": "key-0"}]))
            }
            ("POST", "/api/biz/v1/organization/org-1/projects/p-1/api_keys") => {
                ok(json!({"name": "zcode-api-key", "apiKey": "key-1"}))
            }
            (
                "POST",
                "/api/biz/v1/organization/org-1/projects/p-1/api_keys/key-1/access_tokens",
            ) => {
                let n = self.issued.fetch_add(1, Ordering::SeqCst) + 1;
                let now = NOW_MS / 1000;
                ok(json!({
                    "accessToken": format!("pat-{n}"),
                    "tokenType": "Bearer",
                    "expiresIn": 3600,
                    "expiresAt": now + 3600,
                }))
            }
            ("GET", SUBSCRIPTION) => {
                let mut replies = self.subscriptions.lock().unwrap();
                let reply = if replies.len() > 1 {
                    replies.remove(0)
                } else {
                    let only = &replies[0];
                    Reply::json(only.status, &serde_json::from_str(&only.body).unwrap())
                };
                match self.hold.lock().unwrap().take() {
                    Some((arrived, gate)) => {
                        arrived.notify_one();
                        reply.until(&gate)
                    }
                    None => reply,
                }
            }
            _ => Reply::json(404, &json!({"code": 404})),
        }
    }
}

async fn start(app: Arc<App>, secret: Option<CredentialSecret>) -> (Server, Harness) {
    let server = Server::start({
        let app = app.clone();
        move |request| app.reply(request)
    })
    .await;
    let harness = match secret {
        Some(secret) => Harness::with_secret(&server.url(), secret).await,
        None => Harness::new(&server.url()).await,
    };
    (server, harness)
}

/// 个人 provider 在目录里吗（模型 GLM-5.3）。
async fn listed(harness: &Harness, provider: &str) -> bool {
    harness
        .platform
        .catalog()
        .await
        .models
        .iter()
        .any(|entry| entry.provider == provider && entry.model == models::GLM_5_3_ACCOUNT)
}

fn token(endpoint: &ModelEndpoint) -> &str {
    match &endpoint.credential {
        Credential::ApiKey(key) => key.expose(),
        Credential::None => "",
    }
}

fn family(name: &str) -> Family {
    Family::parse(name).unwrap()
}

/// 请求的（方法，路径，鉴权头）。
fn shape(request: &Captured) -> (String, String, Option<String>) {
    (
        request.method.clone(),
        request.path.clone(),
        request.header("authorization").map(str::to_owned),
    )
}

/// golden 里 App 发出的请求的（方法，路径，鉴权头）。
fn app_shape(request: &Value) -> (String, String, Option<String>) {
    (
        request["method"].as_str().unwrap().to_owned(),
        request["path"].as_str().unwrap().to_owned(),
        request["authorization"].as_str().map(str::to_owned),
    )
}

/// 明文凭据（Node 与 App 也接受没有 `enc:v1:` 前缀的旧值）：zai 的 App 登录，profile id 与 token 可换。
async fn zai_login(harness: &Harness, user: &str, token: &str) {
    harness
        .write_credentials(&json!({
            "oauth:zai:access_token": token,
            "zcodejwttoken": "zcode-jwt",
            "oauth:zai:user_info": json!({"user_id": user, "name": "Dev"}).to_string(),
            "oauth:active_provider": "zai",
        }))
        .await;
}

#[tokio::test]
async fn an_app_login_alone_lists_the_individual_plan() {
    for name in ["zai", "bigmodel"] {
        let login = &golden()["logins"][name];
        let (server, harness) = start(App::new(vec![subscribed()]), Some(golden_secret())).await;
        // App 写的文件原文（加密），只有 App 的键：没有 `account-provider:*`。
        let file = login["loginFile"].as_str().unwrap();
        write(harness.paths.credentials().as_path(), file).await;
        let provider = provider(name);
        assert!(listed(&harness, &provider).await, "{name}: listed");
        let endpoint = harness
            .platform
            .endpoint(&selection(&provider), None)
            .await
            .unwrap();
        // 取目录时为订阅查询换到的令牌，第一次请求直接用。
        assert_eq!(token(&endpoint), "pat-1", "{name}");

        // 请求序列与 App 请求期的个人换取相同（方法、路径、鉴权头），之后是订阅列表（鉴权是项目令牌，App 相同）。团队
        // 列表同时也在查（App 的登录有 profile id 与 token，§9.1）：它的 pricing 与 customerInfo 不算，customerInfo 两边
        // 一样，单独数。
        let personal = |request: &(String, String, Option<String>)| {
            request.1 != CUSTOMER && request.1 != PRICING
        };
        let mut expected: Vec<_> = login["locationRequests"]
            .as_array()
            .unwrap()
            .iter()
            .map(app_shape)
            .collect();
        let app_subscription = &golden()["subscriptions"]["available"][name]["requests"][0];
        expected.push(app_shape(app_subscription));
        let customer = expected[0].clone();
        assert_eq!(customer.1, CUSTOMER);
        let requests = server.requests();
        let actual: Vec<_> = requests.iter().map(shape).filter(personal).collect();
        let expected: Vec<_> = expected.into_iter().filter(personal).collect();
        assert_eq!(actual, expected, "{name}");
        let customers: Vec<_> = requests
            .iter()
            .map(shape)
            .filter(|request| request.1 == CUSTOMER)
            .collect();
        assert_eq!(customers, [customer.clone(), customer], "{name}");
        let create = requests.iter().find(|r| r.method == "POST").unwrap();
        assert_eq!(
            create.json(),
            login["locationRequests"][2]["body"],
            "{name}"
        );

        // 项目位置：键的算法与 App 逐字节相同（profile id 来自 App 写的 user_info），值也相同。
        let profile = login["profileId"].as_str().unwrap();
        let origin = login["origin"].as_str().unwrap();
        assert_eq!(
            storage_key(origin, family(name), profile, None),
            login["location"]["key"].as_str().unwrap(),
            "{name}"
        );
        let ours = storage_key(&server.url(), family(name), profile, None);
        let credentials = harness.credentials().await;
        assert_eq!(
            credentials.present(&ours).map(|v| v.expose().to_owned()),
            login["location"]["value"].as_str().map(str::to_owned),
            "{name}"
        );
        // 不替 App 写键：文件里只有 App 写的键与新的项目位置，App 的值不变。
        let record = login["loginRecord"].as_object().unwrap();
        let raw: Value = serde_json::from_str(
            &crate::test_support::read_text(harness.paths.credentials().as_path())
                .await
                .unwrap(),
        )
        .unwrap();
        let mut keys: Vec<&String> = raw.as_object().unwrap().keys().collect();
        keys.sort();
        let mut expected_keys: Vec<&String> = record.keys().chain([&ours]).collect();
        expected_keys.sort();
        assert_eq!(keys, expected_keys, "{name}");
        for (key, value) in record {
            assert_eq!(
                credentials.present(key).map(|v| v.expose().to_owned()),
                value.as_str().map(|v| v.trim().to_owned()),
                "{name}: {key}"
            );
        }
    }
}

#[tokio::test]
async fn an_app_location_is_reused() {
    for name in ["zai", "bigmodel"] {
        let login = &golden()["logins"][name];
        let (server, harness) = start(App::new(vec![subscribed()]), Some(golden_secret())).await;
        write(
            harness.paths.credentials().as_path(),
            login["loginFile"].as_str().unwrap(),
        )
        .await;
        // App 已建的位置（值取 App 写的；键按同一算法，地址换成本地假服务）。
        let profile = login["profileId"].as_str().unwrap();
        let key = storage_key(&server.url(), family(name), profile, None);
        let value = login["location"]["value"].as_str().unwrap().to_owned();
        harness
            .store()
            .mutate(|credentials| credentials.set(&key, &value))
            .await
            .unwrap();
        assert!(listed(&harness, &provider(name)).await, "{name}");
        // 个人不再查 customerInfo 与 key 列表：只签发与查订阅（App 的新进程也是这两步，golden `reuse`）。团队列表的
        // pricing 与 customerInfo 照常（一次 customerInfo）。
        let paths: Vec<String> = server
            .requests()
            .iter()
            .map(|r| r.path.clone())
            .filter(|path| path != CUSTOMER && path != PRICING)
            .collect();
        let issue = login["reuse"]["requests"][0]["path"].as_str().unwrap();
        assert_eq!(paths, [issue, SUBSCRIPTION], "{name}");
        assert_eq!(server.count(CUSTOMER), 1, "{name}");
    }
}

#[tokio::test]
async fn a_node_cli_identity_is_neither_needed_nor_enough() {
    let (server, harness) = start(App::new(vec![subscribed()]), None).await;
    // Node 独立 CLI 的身份与来源、有 token，没有 user_info：照 App 没有 profile id，不可用，不联网。
    let zai = provider("zai");
    harness
        .write_credentials(&json!({
            format!("account-provider:{zai}:identity"): "u-1",
            format!("account-provider:{zai}:auth-source"): "oauth",
            "oauth:zai:access_token": "jwt-zai",
        }))
        .await;
    assert!(!listed(&harness, &zai).await);
    assert!(
        harness
            .platform
            .endpoint(&selection(&zai), None)
            .await
            .is_err()
    );
    assert_eq!(server.requests().len(), 0);
    // 只有 App 的键（没有身份与来源）：可用，请求期也不补写来源。
    zai_login(&harness, "u-1", "jwt-zai").await;
    assert!(listed(&harness, &zai).await);
    harness
        .platform
        .endpoint(&selection(&zai), None)
        .await
        .unwrap();
    let credentials = harness.credentials().await;
    assert!(
        credentials
            .present(&format!("account-provider:{zai}:auth-source"))
            .is_none()
    );
    assert!(
        credentials
            .present(&format!("account-provider:{zai}:identity"))
            .is_none()
    );
}

#[tokio::test]
async fn the_subscription_decides_and_unknown_keeps_the_last_result() {
    let app = App::new(vec![Reply::json(500, &json!({}))]);
    let (server, harness) = start(app.clone(), None).await;
    let zai = provider("zai");
    // 第一次 unknown：沿用上次，第一次是 false。
    zai_login(&harness, "u-1", "jwt-1").await;
    assert!(!listed(&harness, &zai).await);
    // unknown 不记：下一次重建（同样的凭据重写一次）再查，这次可用。
    app.script(vec![subscribed()]);
    zai_login(&harness, "u-1", "jwt-1").await;
    assert!(listed(&harness, &zai).await);
    // 可用记下了：同一 profile id 与 token 的重建不再查。
    zai_login(&harness, "u-1", "jwt-1").await;
    assert!(listed(&harness, &zai).await);
    assert_eq!(server.count(SUBSCRIPTION), 2);
    // 换 token（同一个 profile id）：重查；unknown 沿用上次的“可用”。
    app.script(vec![Reply::json(503, &json!({}))]);
    zai_login(&harness, "u-1", "jwt-2").await;
    assert!(listed(&harness, &zai).await);
    assert_eq!(server.count(SUBSCRIPTION), 3);
    // 换账号（profile id 变了）：unknown 不沿用别的账号的结果。
    zai_login(&harness, "u-2", "jwt-3").await;
    assert!(!listed(&harness, &zai).await);
    // 没有订阅（过期）：不可用。
    app.script(vec![Reply::json(
        200,
        &json!({"code": 200, "data": [{"productName": "GLM Coding Plan", "status": "EXPIRED", "inCurrentPeriod": false}]}),
    )]);
    zai_login(&harness, "u-2", "jwt-3").await;
    assert!(!listed(&harness, &zai).await);
    // 鉴权失败（401）：不可用。
    app.script(vec![Reply::json(401, &json!({"code": 401}))]);
    zai_login(&harness, "u-2", "jwt-4").await;
    assert!(!listed(&harness, &zai).await);
    // 可用之后退出：记录删掉，同一个账号再登录、这次 unknown，从 false 开始（App 的范围变过）。
    app.script(vec![subscribed()]);
    zai_login(&harness, "u-2", "jwt-5").await;
    assert!(listed(&harness, &zai).await);
    harness.write_credentials(&json!({"other": "kept"})).await;
    assert!(!listed(&harness, &zai).await);
    app.script(vec![Reply::json(500, &json!({}))]);
    let before = server.count(SUBSCRIPTION);
    zai_login(&harness, "u-2", "jwt-5").await;
    assert!(!listed(&harness, &zai).await);
    assert_eq!(server.count(SUBSCRIPTION), before + 1);
}

#[tokio::test]
async fn an_account_change_during_the_query_discards_the_round() {
    let app = App::new(vec![subscribed()]);
    let (server, harness) = start(app.clone(), None).await;
    let harness = Arc::new(harness);
    let (arrived, gate) = (Arc::new(tokio::sync::Notify::new()), Gate::default());
    zai_login(&harness, "u-1", "jwt-1").await;
    // 登录之后再布置：只拦这一轮查询的订阅请求，换完账号再放行（F123）。
    *app.hold.lock().unwrap() = Some((arrived.clone(), gate.clone()));
    let pending = tokio::spawn({
        let harness = harness.clone();
        async move { harness.platform.catalog().await }
    });
    arrived.notified().await;
    zai_login(&harness, "u-1", "jwt-2").await;
    gate.open();
    // 这一轮丢弃：没有发布任何快照。
    let first = pending.await.unwrap();
    assert_eq!(first.revision, 0);
    assert!(first.models.is_empty());
    // 下一次按新 token 查。
    assert!(listed(&harness, &provider("zai")).await);
    // customerInfo 由个人换取与团队列表各取一次：按 token 去掉相邻的重复。
    let mut tokens: Vec<String> = server
        .requests()
        .iter()
        .filter(|r| r.path == CUSTOMER)
        .filter_map(|r| r.header("authorization").map(str::to_owned))
        .collect();
    tokens.dedup();
    assert_eq!(tokens, ["jwt-1", "jwt-2"]);
}

#[tokio::test]
async fn a_manual_key_wins_over_the_app_login_without_a_subscription_check() {
    let (server, harness) = start(App::new(vec![subscribed()]), Some(golden_secret())).await;
    write(
        harness.paths.credentials().as_path(),
        golden()["logins"]["zai"]["loginFile"].as_str().unwrap(),
    )
    .await;
    let zai = provider("zai");
    let identity = "key-0123456789abcdef01234567";
    harness
        .store()
        .mutate(|credentials| {
            credentials.set(&format!("account-provider:{zai}:identity"), identity)?;
            credentials.set(&format!("account-provider:{zai}:auth-source"), "manual")?;
            credentials.set(
                &format!("account-provider:coding-plan:{zai}:account:{identity}:api-key"),
                "sk-manual",
            )
        })
        .await
        .unwrap();
    let endpoint = harness
        .platform
        .endpoint(&selection(&zai), None)
        .await
        .unwrap();
    assert_eq!(token(&endpoint), "sk-manual");
    // 个人不查订阅、不换项目令牌（团队列表照常查，与个人无关）。
    assert_eq!(server.count(SUBSCRIPTION), 0);
    assert_eq!(server.count("/api/biz/v1/"), 0);
}

#[tokio::test]
async fn a_rejected_token_is_not_retried_after_the_zcode_jwt_changed() {
    let (_server, harness) = start(App::new(vec![subscribed()]), None).await;
    let zai = provider("zai");
    zai_login(&harness, "u-1", "jwt-1").await;
    let first = harness
        .platform
        .endpoint(&selection(&zai), None)
        .await
        .unwrap();
    assert_eq!(token(&first), "pat-1");
    // App 的账号范围含 zcodejwttoken：它变了之后，旧令牌的 401 不拿新范围重发。
    harness
        .store()
        .mutate(|credentials| credentials.set("zcodejwttoken", "zcode-jwt-2"))
        .await
        .unwrap();
    let message = match harness
        .platform
        .endpoint(&selection(&zai), Some(&first))
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
