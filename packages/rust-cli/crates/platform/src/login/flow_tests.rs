//! 登录与退出（platform.md §5.6），对本地假服务；个人配置的写入与 Node 生成的夹具逐字节相同。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::provider::{Credential, ModelDirectory, ModelEndpoints};
use serde_json::{Value, json};

use super::LoginError;
use crate::fake_server_tests::{
    Account, BIGMODEL, Captured, Harness, NOW_MS, Reply, Server, ZAI, selection,
};
use crate::test_support::{read_text, write};
use crate::token::{Family, TokenError};

const EMPTY_CONFIG: &str = include_str!("../../tests/fixtures/login-config-empty.json");
const NODE_CONFIG: &str = include_str!("../../tests/fixtures/login-config-node.json");
const NODE_CONFIG_ZAI: &str = include_str!("../../tests/fixtures/login-config-node-zai.json");

fn init(expires_in: u64) -> Reply {
    Reply::json(
        200,
        &json!({"code": 0, "msg": "ok", "data": {
            "flow_id": "flow/1",
            "authorize_url": "https://chat.example.com/oauth?flow=1",
            "expires_at": NOW_MS / 1000 + expires_in,
            "poll_interval_sec": 1,
        }}),
    )
}

fn ready(family: &str) -> Reply {
    Reply::json(
        200,
        &json!({"code": 0, "data": {
            "status": "ready",
            "token": "zcode-jwt",
            "user": {"user_id": " u-9 ", "name": "Dev", "email": "dev@example.com", "extra": 1},
            family: {"access_token": "at-new", "refresh_token": "rt-new"},
        }}),
    )
}

/// 登录服务：init、按 `polls` 依次回复轮询，其余交给账号服务。
async fn service(polls: Vec<Reply>) -> (Server, Arc<Account>) {
    let account = Account::new(true);
    let polls = Arc::new(std::sync::Mutex::new(
        polls.into_iter().rev().collect::<Vec<_>>(),
    ));
    let server = Server::start({
        let account = account.clone();
        move |request: &Captured| match request.path.as_str() {
            "/api/v1/oauth/cli/init" => init(600),
            path if path.starts_with("/api/v1/oauth/cli/poll/") => {
                polls.lock().unwrap().pop().unwrap_or_else(|| {
                    Reply::json(200, &json!({"code": 0, "data": {"status": "pending"}}))
                })
            }
            _ => account.reply(request),
        }
    })
    .await;
    (server, account)
}

fn text(harness_value: Option<RedactedString>) -> String {
    harness_value
        .map(|value| value.expose().to_owned())
        .unwrap_or_default()
}

#[tokio::test]
async fn zai_login_polls_exchanges_and_writes_both_files() {
    let pending = Reply::json(200, &json!({"code": 0, "data": {"status": "pending"}}));
    let (server, account) =
        service(vec![Reply::json(503, &json!({})), pending, ready("zai")]).await;
    let harness = Harness::new(&server.url()).await;
    let login = harness.platform.begin_login(Family::Zai).await.unwrap();
    assert_eq!(
        login.authorize_url(),
        "https://chat.example.com/oauth?flow=1"
    );
    let outcome = login.complete().await.unwrap();
    assert_eq!(outcome.model, "account:zai-individual-coding-plan/GLM-5.3");
    assert_eq!(outcome.user_label(), Some("Dev"));
    assert_eq!(
        outcome.credentials.as_path(),
        harness.paths.credentials().as_path()
    );

    let requests = server.requests();
    assert_eq!(requests[0].path, "/api/v1/oauth/cli/init");
    assert_eq!(requests[0].json(), json!({"provider": "zai"}));
    let poll_auth = requests[0].header("authorization").unwrap().to_owned();
    let poll_token = poll_auth.strip_prefix("Bearer ").unwrap();
    assert!(poll_token.len() == 64 && poll_token.bytes().all(|b| b.is_ascii_hexdigit()));
    let polls: Vec<&Captured> = requests
        .iter()
        .filter(|r| r.path.contains("/poll/"))
        .collect();
    assert_eq!(polls.len(), 3);
    assert!(
        polls
            .iter()
            .all(|r| r.path == "/api/v1/oauth/cli/poll/flow%2F1"
                && r.method == "GET"
                && r.header("authorization") == Some(poll_auth.as_str()))
    );
    // 登录时只用 OAuth token 换业务 JWT（App `normalizePolledTokenSet`）。F50 追加：不再换项目令牌验证账号（照 App；
    // 以前照 Node 独立 CLI 换一次）。
    let login = requests
        .iter()
        .position(|r| r.path == "/api/auth/z/login")
        .unwrap();
    assert_eq!(requests[login].json(), json!({"token": "at-new"}));
    assert_eq!(account.business.load(Ordering::SeqCst), 1);
    assert_eq!(account.issued.load(Ordering::SeqCst), 0);
    assert!(requests.iter().all(|r| !r.path.starts_with("/api/biz/")));
    let business = crate::fake_server_tests::jwt(crate::fake_server_tests::NOW_MS / 1000 + 7200, 1);

    let credentials = harness.credentials().await;
    // 修复线 F47：存的是业务 JWT（App 的语义）；以前按 Node 独立 CLI 存原始 OAuth token `at-new`。工作线 F50：user_info
    // 照 App 存后端的原始用户对象（以前按 Node 只留 avatar / email / name / user_id 并去空白），不写身份与来源。
    for (key, value) in [
        ("oauth:active_provider", "zai"),
        ("oauth:zai:access_token", business.as_str()),
        ("zcodejwttoken", "zcode-jwt"),
        (
            "oauth:zai:user_info",
            r#"{"user_id":" u-9 ","name":"Dev","email":"dev@example.com","extra":1}"#,
        ),
    ] {
        assert_eq!(text(credentials.present(key)), value, "{key}");
    }
    assert!(credentials.present("oauth:zai:refresh_token").is_none());
    for key in [
        "account-provider:account:zai-individual-coding-plan:identity",
        "account-provider:account:zai-individual-coding-plan:auth-source",
    ] {
        assert!(credentials.present(key).is_none(), "{key}");
    }
    let config = read_text(harness.paths.provider_config().as_path())
        .await
        .unwrap();
    assert_eq!(config, EMPTY_CONFIG);

    // 登录之后账号模型可用（取目录时换项目令牌、查个人订阅，§5.7），默认选择是它；请求用取目录时换到的令牌。
    let catalog = harness.platform.catalog().await;
    assert_eq!(
        catalog.default.as_ref().map(|s| s.provider.as_str()),
        Some(ZAI)
    );
    let endpoint = harness
        .platform
        .endpoint(&selection(ZAI), None)
        .await
        .unwrap();
    assert_eq!(
        endpoint.credential,
        Credential::ApiKey(RedactedString::new("pat-1"))
    );
    // 请求期直接用存好的业务 JWT，不再换。
    assert_eq!(account.business.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn bigmodel_login_writes_the_app_profile() {
    let (server, _account) = service(vec![ready("bigmodel")]).await;
    let harness = Harness::new(&server.url()).await;
    write(harness.paths.provider_config().as_path(), NODE_CONFIG).await;
    let outcome = harness
        .platform
        .begin_login(Family::Bigmodel)
        .await
        .unwrap()
        .complete()
        .await
        .unwrap();
    assert_eq!(
        outcome.model,
        "account:bigmodel-individual-coding-plan/GLM-5.3"
    );
    let credentials = harness.credentials().await;
    // 工作线 F50：照 App 的轮询登录，profile id 是去空白的 `user_id`，rawProfile 是原始用户对象加 App 的版本号；
    // 以前按 Node 独立 CLI 用 `key-` + sha256(JSON [组织, 项目]) 作 id，并写身份与来源。
    let info = r#"{"id":"u-9","username":"Dev","displayName":"Dev","rawProfile":{"user_id":" u-9 ","name":"Dev","email":"dev@example.com","extra":1,"zcodeProfileSchemaVersion":2}}"#;
    for (key, value) in [
        ("oauth:active_provider", "bigmodel"),
        ("zcodejwttoken", "zcode-jwt"),
        ("oauth:bigmodel:access_token", "at-new"),
        ("oauth:bigmodel:refresh_token", "rt-new"),
        ("oauth:bigmodel:user_info", info),
    ] {
        assert_eq!(text(credentials.present(key)), value, "{key}");
    }
    for key in [
        "account-provider:account:bigmodel-individual-coding-plan:identity",
        "account-provider:account:bigmodel-individual-coding-plan:auth-source",
    ] {
        assert!(credentials.present(key).is_none(), "{key}");
    }
    // 已有的 Node 规范文件：只改默认选择，其余字节与 Node 相同。
    let config = read_text(harness.paths.provider_config().as_path())
        .await
        .unwrap();
    assert_eq!(config, NODE_CONFIG);
    harness
        .platform
        .configure_api_key(Family::Zai, &RedactedString::new("sk-zai"))
        .await
        .unwrap();
    let config = read_text(harness.paths.provider_config().as_path())
        .await
        .unwrap();
    assert_eq!(config, NODE_CONFIG_ZAI);
}

#[tokio::test]
async fn failed_expired_and_rejected_logins_write_nothing() {
    let failed = Reply::json(200, &json!({"code": 0, "data": {"status": "failed"}}));
    let (server, _account) = service(vec![failed]).await;
    let harness = Harness::new(&server.url()).await;
    let error = harness
        .platform
        .begin_login(Family::Zai)
        .await
        .unwrap()
        .complete()
        .await;
    assert_eq!(error.unwrap_err(), LoginError::Failed);
    assert_eq!(
        LoginError::Failed.to_string(),
        "Authorization failed. Please retry login."
    );

    let (server, _account) = service(vec![Reply::json(400, &json!({}))]).await;
    let harness = Harness::new(&server.url()).await;
    let error = harness
        .platform
        .begin_login(Family::Zai)
        .await
        .unwrap()
        .complete()
        .await;
    assert_eq!(
        error.unwrap_err(),
        LoginError::OAuth("OAuth HTTP error 400".to_owned())
    );

    // 授权已过期：不轮询。
    let server = Server::start(|request: &Captured| match request.path.as_str() {
        "/api/v1/oauth/cli/init" => init(0),
        _ => Reply::json(500, &json!({})),
    })
    .await;
    let harness = Harness::new(&server.url()).await;
    assert_eq!(
        harness.platform.begin_login(Family::Zai).await.err(),
        Some(LoginError::Timeout)
    );
    // 业务码非 0：msg 原样给出。
    let server = Server::start(|_: &Captured| {
        Reply::json(200, &json!({"code": 1001, "msg": "provider disabled"}))
    })
    .await;
    let harness = Harness::new(&server.url()).await;
    assert_eq!(
        harness.platform.begin_login(Family::Zai).await.err(),
        Some(LoginError::OAuth("provider disabled".to_owned()))
    );

    // zai 的业务 JWT 换不到：不写任何文件（F50 追加：登录不再换项目令牌，以前这里测的是项目令牌换取失败）。
    let server = Server::start(move |request: &Captured| match request.path.as_str() {
        "/api/v1/oauth/cli/init" => init(600),
        path if path.starts_with("/api/v1/oauth/cli/poll/") => ready("zai"),
        "/api/auth/z/login" => Reply::json(401, &json!({"code": 401})),
        _ => Reply::json(404, &json!({"code": 404})),
    })
    .await;
    let harness = Harness::new(&server.url()).await;
    let error = harness
        .platform
        .begin_login(Family::Zai)
        .await
        .unwrap()
        .complete()
        .await;
    assert_eq!(
        error.unwrap_err(),
        LoginError::Token(TokenError::LoginFailed)
    );
    assert!(
        read_text(harness.paths.credentials().as_path())
            .await
            .is_none()
    );
    assert!(
        read_text(harness.paths.provider_config().as_path())
            .await
            .is_none()
    );
}

/// 只有团队项目、没有个人项目的账号（F50 追加，用户决定 2026-10-21，照 App）：登录成功、登录时不发 `/api/biz/` 请求；
/// 之后团队套餐可用，个人套餐不可用（换不到个人项目令牌，订阅是 unknown）。以前登录时换个人项目令牌，报
/// `project_token_project_unavailable`、不写文件。
#[tokio::test]
async fn a_team_only_account_logs_in_and_uses_the_team_plan() {
    let now = NOW_MS / 1000;
    let server = Server::start(move |request: &Captured| {
        let ok = |data: Value| Reply::json(200, &json!({"code": 200, "data": data}));
        match (request.method.as_str(), request.path.as_str()) {
            (_, "/api/v1/oauth/cli/init") => init(600),
            (_, path) if path.starts_with("/api/v1/oauth/cli/poll/") => ready("bigmodel"),
            ("GET", "/api/biz/customer/getCustomerInfo") => ok(json!({"organizations": [
                {"organizationId": "org-t", "organizationName": "团队", "projects": [
                    {"projectId": "p-t", "projectName": "默认项目", "projectType": 2}
                ]}
            ]})),
            ("GET", "/api/biz/subscription/enterprise/v2/pricing") => ok(json!({"productList": [
                {"productId": "prod-1", "tier": "PRO", "subscribed": true, "organizationId": "org-t"}
            ]})),
            ("GET", "/api/biz/team/subscribe/product/querySubscribeDetail") => ok(json!({
                "hasSubscription": true, "status": "EFFECTIVE", "memberGrantStatus": "VALID"
            })),
            ("GET", "/api/biz/v1/organization/org-t/projects/p-t/api_keys") => ok(json!([
                {"name": "zcode-team-api-key", "apiKey": "key-team", "keyType": 2}
            ])),
            ("POST", "/api/biz/v1/organization/org-t/projects/p-t/api_keys/key-team/access_tokens") => {
                ok(json!({
                    "accessToken": "pat-team", "tokenType": "Bearer",
                    "expiresIn": 3600, "expiresAt": now + 3600,
                }))
            }
            _ => Reply::json(404, &json!({"code": 404})),
        }
    })
    .await;
    let harness = Harness::new(&server.url()).await;
    harness
        .platform
        .begin_login(Family::Bigmodel)
        .await
        .unwrap()
        .complete()
        .await
        .unwrap();
    assert_eq!(server.count("/api/biz/"), 0);
    let credentials = harness.credentials().await;
    assert_eq!(
        text(credentials.present("oauth:bigmodel:access_token")),
        "at-new"
    );
    let catalog = harness.platform.catalog().await;
    let team = "account:bigmodel-team-coding-plan";
    assert!(catalog.models.iter().any(|m| m.provider == team));
    assert!(!catalog.models.iter().any(|m| m.provider == BIGMODEL));
    // 新会话的缺省落到第一个团队项目（登录写的个人缺省不可选，§9.3），请求用团队项目的令牌。
    let default = catalog.default.unwrap();
    assert_eq!(default.provider, team);
    let endpoint = harness.platform.endpoint(&default, None).await.unwrap();
    assert_eq!(
        endpoint.credential,
        Credential::ApiKey(RedactedString::new("pat-team"))
    );
}

#[tokio::test]
async fn api_keys_and_logout_follow_node() {
    let calls = Arc::new(AtomicUsize::new(0));
    let server = Server::start({
        let calls = calls.clone();
        move |_: &Captured| {
            calls.fetch_add(1, Ordering::SeqCst);
            Reply::json(500, &json!({}))
        }
    })
    .await;
    let harness = Harness::new(&server.url()).await;
    harness
        .write_credentials(&json!({"unrelated": "keep", "oauth:zai:access_token": "at-old"}))
        .await;
    assert_eq!(
        harness
            .platform
            .configure_api_key(Family::Zai, &RedactedString::new("  "))
            .await
            .err(),
        Some(LoginError::EmptyApiKey)
    );
    let outcome = harness
        .platform
        .configure_api_key(Family::Zai, &RedactedString::new(" sk-manual "))
        .await
        .unwrap();
    assert_eq!(outcome.model, "account:zai-individual-coding-plan/GLM-5.3");
    let credentials = harness.credentials().await;
    // Node：key- + sha256("sk-manual") 的前 24 个 hex。
    let identity = "key-1e5d18ec8e47b2df74173352";
    assert_eq!(
        text(credentials.present(&format!("account-provider:{ZAI}:identity"))),
        identity
    );
    assert_eq!(
        text(credentials.present(&format!("account-provider:{ZAI}:auth-source"))),
        "manual"
    );
    assert_eq!(
        text(credentials.present(&format!(
            "account-provider:coding-plan:{ZAI}:account:{identity}:api-key"
        ))),
        "sk-manual"
    );
    let endpoint = harness
        .platform
        .endpoint(&selection(ZAI), None)
        .await
        .unwrap();
    assert_eq!(
        endpoint.credential,
        Credential::ApiKey(RedactedString::new("sk-manual"))
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "manual keys are not checked online"
    );

    harness
        .platform
        .configure_api_key(Family::Bigmodel, &RedactedString::new("sk-big"))
        .await
        .unwrap();
    let path = harness.platform.logout().await.unwrap();
    assert_eq!(path.as_path(), harness.paths.credentials().as_path());
    let record: Value = serde_json::from_str(&read_text(path.as_path()).await.unwrap()).unwrap();
    let keys: Vec<&String> = record.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["unrelated"]);
    assert!(
        !harness
            .platform
            .catalog()
            .await
            .models
            .iter()
            .any(|m| m.provider == ZAI || m.provider == BIGMODEL)
    );
}

#[tokio::test]
async fn invalid_personal_configs_are_not_overwritten() {
    let server = Server::start(|_: &Captured| Reply::json(500, &json!({}))).await;
    let harness = Harness::new(&server.url()).await;
    let invalid = r#"{"schemaVersion":2,"config":{}}"#;
    write(harness.paths.provider_config().as_path(), invalid).await;
    let error = harness
        .platform
        .configure_api_key(Family::Zai, &RedactedString::new("sk"))
        .await
        .unwrap_err();
    assert!(matches!(error, LoginError::ConfigUpdate(_)));
    let config = read_text(harness.paths.provider_config().as_path())
        .await
        .unwrap();
    assert_eq!(config, invalid);
}

// ---- 工作线 F50：登录与退出写的键照 App（platform.md §5.6），与 App 生成的 golden 对拍 ----

fn app_golden() -> Value {
    serde_json::from_str(include_str!("../../tests/fixtures/app-login-golden.json")).unwrap()
}

/// golden 里 App 轮询登录时的回复（同一份用户、token），再加上本工程登录时验证账号的项目令牌换取。
async fn app_service(family: &'static str) -> Server {
    let golden = app_golden();
    let user = golden["logins"][family]["pollUser"].clone();
    let customer = golden["customer"].clone();
    Server::start(move |request: &Captured| {
        let ok = |data: Value| Reply::json(200, &json!({"code": 200, "data": data}));
        match (request.method.as_str(), request.path.as_str()) {
            (_, "/api/v1/oauth/cli/init") => init(600),
            (_, path) if path.starts_with("/api/v1/oauth/cli/poll/") => {
                let tokens = if family == "zai" {
                    json!({"zai": {"access_token": "zai-oauth"}})
                } else {
                    json!({"bigmodel": {"access_token": "at-big", "refresh_token": "rt-big"}})
                };
                let mut data = json!({"status": "ready", "token": " zcode-jwt ", "user": user});
                data.as_object_mut()
                    .unwrap()
                    .extend(tokens.as_object().unwrap().clone());
                Reply::json(200, &json!({"code": 0, "data": data}))
            }
            ("POST", "/api/auth/z/login") => Reply::json(
                200,
                &json!({"code": 0, "data": {"access_token": "jwt-zai"}}),
            ),
            ("GET", "/api/biz/customer/getCustomerInfo") => ok(customer.clone()),
            ("GET", path) if path.ends_with("/api_keys") => ok(json!([])),
            ("POST", path) if path.ends_with("/api_keys") => {
                ok(json!({"name": "zcode-api-key", "apiKey": "key-1"}))
            }
            ("POST", path) if path.ends_with("/access_tokens") => ok(json!({
                "accessToken": "pat-1", "tokenType": "Bearer",
                "expiresIn": 3600, "expiresAt": NOW_MS / 1000 + 3600,
            })),
            _ => Reply::json(404, &json!({"code": 404})),
        }
    })
    .await
}

#[tokio::test]
async fn login_writes_what_the_app_writes() {
    for (name, family) in [("zai", Family::Zai), ("bigmodel", Family::Bigmodel)] {
        let server = app_service(name).await;
        let harness = Harness::new(&server.url()).await;
        harness
            .platform
            .begin_login(family)
            .await
            .unwrap()
            .complete()
            .await
            .unwrap();
        // 键与值都与 App 的轮询登录写的相同（golden 解密后的记录），没有别的键（不写身份、来源与项目位置）。
        let app = app_golden()["logins"][name]["loginRecord"].clone();
        let app = app.as_object().unwrap();
        let raw: Value = serde_json::from_str(
            &read_text(harness.paths.credentials().as_path())
                .await
                .unwrap(),
        )
        .unwrap();
        // 登录时发的请求与 App 的轮询登录相同（init、poll，zai 加业务 JWT；轮询的 flow ID 不比），没有 `/api/biz/`。
        let poll = |path: &str| path.starts_with("/api/v1/oauth/cli/poll/");
        let sent: Vec<(String, String)> = server
            .requests()
            .iter()
            .filter(|r| !poll(&r.path))
            .map(|r| (r.method.clone(), r.path.clone()))
            .collect();
        let app_sent: Vec<(String, String)> = app_golden()["logins"][name]["loginRequests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                let path = r["path"].as_str().unwrap().to_owned();
                (r["method"].as_str().unwrap().to_owned(), path)
            })
            .filter(|(_, path)| !poll(path))
            .collect();
        assert_eq!(sent, app_sent, "{name}");
        let mut ours: Vec<&String> = raw.as_object().unwrap().keys().collect();
        let mut theirs: Vec<&String> = app.keys().collect();
        ours.sort();
        theirs.sort();
        assert_eq!(ours, theirs, "{name}");
        let credentials = harness.credentials().await;
        for (key, value) in app {
            assert_eq!(
                text(credentials.present(key)),
                value.as_str().unwrap(),
                "{name}: {key}"
            );
        }
    }
}

#[tokio::test]
async fn an_oauth_login_replaces_a_manual_connection() {
    let (server, _account) = service(vec![ready("zai")]).await;
    let harness = Harness::new(&server.url()).await;
    harness
        .platform
        .configure_api_key(Family::Zai, &RedactedString::new("sk-manual"))
        .await
        .unwrap();
    let manual = harness
        .platform
        .endpoint(&selection(ZAI), None)
        .await
        .unwrap();
    assert_eq!(
        manual.credential,
        Credential::ApiKey(RedactedString::new("sk-manual"))
    );
    harness
        .platform
        .begin_login(Family::Zai)
        .await
        .unwrap()
        .complete()
        .await
        .unwrap();
    // 手工 key 的来源会盖过 OAuth：OAuth 登录删掉这个 provider 的身份、来源与那份 key（Node 写 `auth-source = oauth`）。
    let raw: Value = serde_json::from_str(
        &read_text(harness.paths.credentials().as_path())
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(
        raw.as_object()
            .unwrap()
            .keys()
            .all(|key| !key.starts_with("account-provider:")),
        "{raw}"
    );
    let endpoint = harness
        .platform
        .endpoint(&selection(ZAI), None)
        .await
        .unwrap();
    assert!(matches!(
        &endpoint.credential,
        Credential::ApiKey(key) if key.expose().starts_with("pat-")
    ));
}

#[tokio::test]
async fn logout_removes_what_the_app_removes() {
    let server = Server::start(|_: &Captured| Reply::json(500, &json!({}))).await;
    let harness = Harness::new(&server.url()).await;
    // App `OAuthService.logoutAll` 之前的记录（golden）：两个 family 的登录、App 按 profile id 存过的派生 key、
    // 别的账号的派生 key、项目位置与无关的键。
    let golden = app_golden();
    harness.write_credentials(&golden["logout"]["record"]).await;
    harness.platform.logout().await.unwrap();
    let raw: Value = serde_json::from_str(
        &read_text(harness.paths.credentials().as_path())
            .await
            .unwrap(),
    )
    .unwrap();
    let mut keys: Vec<String> = raw.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    let expected: Vec<String> = golden["logout"]["remaining"]
        .as_array()
        .unwrap()
        .iter()
        .map(|key| key.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(keys, expected);
}

/// 轮询的响应超过 64 KiB：照 Node（`readResponseBody` 拒收、`auth-login-polling` 按间隔重试）。修复线 F58：以前截断到
/// 64 KiB 再解析，报“不是 JSON”，当作不可重试的错误，登录立即失败。
#[tokio::test]
async fn an_oversized_poll_reply_is_rejected_and_retried() {
    let oversized = Reply::json(
        200,
        &json!({"code": 0, "data": {"status": "pending", "pad": "x".repeat(70_000)}}),
    );
    let (server, _account) = service(vec![oversized, ready("zai")]).await;
    let harness = Harness::new(&server.url()).await;
    let login = harness.platform.begin_login(Family::Zai).await.unwrap();
    let outcome = login.complete().await.unwrap();
    assert_eq!(outcome.model, "account:zai-individual-coding-plan/GLM-5.3");
    let polls = server
        .requests()
        .iter()
        .filter(|request| request.path.contains("/poll/"))
        .count();
    assert_eq!(polls, 2);
}
