//! 项目令牌的换取（platform.md §5.3、§5.4），经平台层的每请求鉴权（`AccountAuth`，不经目录：个人订阅的查询见
//! `personal/`），对本地假服务。zai 存的是业务 JWT（修复线 F47，照 App），请求期直接用、不再换。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::model::ModelError;
use rust_cli_kernel_api::provider::{Credential, ModelEndpoints};
use serde_json::{Value, json};

use crate::fake_server_tests::{Account, BIGMODEL, Gate, Harness, Reply, Server, ZAI, selection};

const ISSUE: &str = "/api/biz/v1/organization/org-1/projects/p-1/api_keys/key-1/access_tokens";

/// 这次请求的个人凭据（`rejected` 是刚被模型服务拒绝的令牌）。
async fn pat(
    harness: &Harness,
    (provider, family): (&str, &str),
    rejected: Option<&str>,
) -> Result<String, ModelError> {
    let rejected = rejected.map(RedactedString::new);
    harness
        .platform
        .auth
        .credential(
            (provider, family, "individual-coding-plan"),
            None,
            rejected.as_ref(),
        )
        .await
        .map(|key| key.expose().to_owned())
}

const ZAI_PLAN: (&str, &str) = (ZAI, "zai");

fn auth_error(result: Result<String, ModelError>) -> String {
    match result {
        Err(ModelError::Authentication { message }) => message,
        other => panic!("expected an authentication error, got {other:?}"),
    }
}

/// App 的 zai 登录（只有 App 写的键：profile id 在 user_info 里，没有 `account-provider:*`）。`access_token` 是存好的
/// 业务 JWT。
async fn zai_login(harness: &Harness, access_token: &str) {
    harness
        .write_credentials(&json!({
            "oauth:zai:user_info": r#"{"user_id":"u-1","name":"Dev"}"#,
            "oauth:zai:access_token": access_token,
        }))
        .await;
}

#[tokio::test]
async fn zai_uses_the_stored_business_jwt_and_caches_a_project_token() {
    let account = Account::new(false);
    let server = Server::start({
        let account = account.clone();
        move |request| account.reply(request)
    })
    .await;
    let harness = Harness::new(&server.url()).await;
    zai_login(&harness, "at-zai").await;
    assert_eq!(pat(&harness, ZAI_PLAN, None).await.unwrap(), "pat-1");

    let requests = server.requests();
    let paths: Vec<(&str, &str)> = requests
        .iter()
        .map(|r| (r.method.as_str(), r.path.as_str()))
        .collect();
    // 修复线 F47：以前先 POST /api/auth/z/login 把存的值当 OAuth token 再换一次业务 JWT。
    assert_eq!(
        paths,
        [
            ("GET", "/api/biz/customer/getCustomerInfo"),
            (
                "GET",
                "/api/biz/v1/organization/org-1/projects/p-1/api_keys"
            ),
            (
                "POST",
                "/api/biz/v1/organization/org-1/projects/p-1/api_keys"
            ),
            ("POST", ISSUE),
        ]
    );
    // customerInfo 用裸业务 JWT；zai 的 key 管理与签发用 Bearer。
    assert_eq!(requests[0].header("authorization"), Some("at-zai"));
    assert_eq!(requests[1].header("authorization"), Some("Bearer at-zai"));
    assert_eq!(
        requests[2].json(),
        json!({"name": "zcode-api-key", "usageScene": 1})
    );
    assert_eq!(requests[3].header("authorization"), Some("Bearer at-zai"));
    assert_eq!(
        requests[3].json(),
        json!({"clientType": "zcode", "clientVersion": "0.1.0-test"})
    );
    assert!(
        requests
            .iter()
            .all(|r| r.header("content-type") == Some("application/json"))
    );

    // 不补写来源（修复线 F50：以前照 Node 独立 CLI 写 `auth-source = oauth`）；项目位置按 App 的 profile id 存进
    // 凭据文件。
    let credentials = harness.credentials().await;
    let source =
        credentials.present("account-provider:account:zai-individual-coding-plan:auth-source");
    assert!(source.is_none());
    let key =
        crate::token::location::storage_key(&server.url(), crate::token::Family::Zai, "u-1", None);
    let saved = credentials.present(&key).unwrap();
    assert_eq!(
        saved.expose(),
        r#"{"organizationId":"org-1","projectId":"p-1","apiKeyId":"key-1"}"#
    );

    // 缓存命中：不再请求。
    assert_eq!(pat(&harness, ZAI_PLAN, None).await.unwrap(), "pat-1");
    assert_eq!(server.requests().len(), 4);

    // 新进程：位置从文件读，只签发。
    let other = Harness::platform(&harness.paths, &server.url(), harness.clock.clone()).await;
    let fresh = other
        .auth
        .credential((ZAI, "zai", "individual-coding-plan"), None, None)
        .await
        .unwrap();
    assert_eq!(fresh.expose(), "pat-2");
    assert_eq!(server.count("/api/biz/customer"), 1);
    assert_eq!(server.count("/api/auth/z/login"), 0);

    // 到了刷新点（到期前 120–180 秒）重新签发。
    harness.advance(3_600_000 - 119_000);
    assert_eq!(pat(&harness, ZAI_PLAN, None).await.unwrap(), "pat-3");

    // 经端点（先过个人订阅的查询，platform.md §5.7）：账号模型的端点可续期，Anthropic 协议另带同一个令牌的 Bearer。
    let endpoint = harness
        .platform
        .endpoint(&selection(ZAI), None)
        .await
        .unwrap();
    let Credential::ApiKey(key) = &endpoint.credential else {
        panic!("an account endpoint carries a project token");
    };
    assert!(key.expose().starts_with("pat-"));
    assert!(endpoint.renewable);
    assert_eq!(
        endpoint.headers[0].1.expose(),
        format!("Bearer {}", key.expose())
    );
}

#[tokio::test]
async fn rejected_tokens_are_replaced_and_late_rejections_are_ignored() {
    let account = Account::new(true);
    let server = Server::start({
        let account = account.clone();
        move |request| account.reply(request)
    })
    .await;
    let harness = Harness::new(&server.url()).await;
    zai_login(&harness, "at-zai").await;
    let first = pat(&harness, ZAI_PLAN, None).await.unwrap();
    // 列表里已有 zcode-api-key：不创建。
    assert_eq!(
        server.count("/api/biz/v1/organization/org-1/projects/p-1/api_keys"),
        2
    );
    let second = pat(&harness, ZAI_PLAN, Some(&first)).await.unwrap();
    assert_eq!((first.as_str(), second.as_str()), ("pat-1", "pat-2"));
    // 迟到的旧 401 不作废新令牌。
    let late = pat(&harness, ZAI_PLAN, Some(&first)).await.unwrap();
    assert_eq!(late, "pat-2");
    assert_eq!(server.count(ISSUE), 2);

    // 换了账号（access token 变了）后，旧账号令牌的 401 不拿新账号重发。
    zai_login(&harness, "at-other").await;
    let message = auth_error(pat(&harness, ZAI_PLAN, Some(&second)).await);
    assert!(
        message.starts_with("project_token_scope_invalidated"),
        "{message}"
    );
    // 不带 rejected 的新请求用新账号换取。
    assert_eq!(pat(&harness, ZAI_PLAN, None).await.unwrap(), "pat-3");
}

#[tokio::test]
async fn concurrent_requests_share_one_exchange() {
    let account = Account::new(true);
    let server = Server::start({
        let account = account.clone();
        move |request| {
            let reply = account.reply(request);
            if request.path == ISSUE {
                reply.after(200)
            } else {
                reply
            }
        }
    })
    .await;
    let harness = Arc::new(Harness::new(&server.url()).await);
    zai_login(&harness, "at-zai").await;
    let mut tasks = Vec::new();
    for _ in 0..5 {
        let harness = harness.clone();
        tasks.push(tokio::spawn(async move {
            pat(&harness, ZAI_PLAN, None).await.unwrap()
        }));
    }
    for task in tasks {
        assert_eq!(task.await.unwrap(), "pat-1");
    }
    assert_eq!(server.count(ISSUE), 1);
    assert_eq!(server.count("/api/auth/z/login"), 0);
}

#[tokio::test]
async fn a_missing_key_forgets_the_location_and_disabled_tokens_fail() {
    let account = Account::new(true);
    let issue_status = Arc::new(std::sync::Mutex::new(vec![404_u16]));
    let server = Server::start({
        let (account, issue_status) = (account.clone(), issue_status.clone());
        move |request| match (request.path == ISSUE).then(|| issue_status.lock().unwrap().pop()) {
            Some(Some(404)) => Reply::json(404, &json!({"code": 404})),
            Some(Some(_)) => Reply::json(200, &json!({"code": 200, "data": {"enable": false}})),
            _ => account.reply(request),
        }
    })
    .await;
    let harness = Harness::new(&server.url()).await;
    zai_login(&harness, "at-zai").await;
    let message = auth_error(pat(&harness, ZAI_PLAN, None).await);
    assert!(
        message.starts_with("project_token_invalid_response"),
        "{message}"
    );
    let key =
        crate::token::location::storage_key(&server.url(), crate::token::Family::Zai, "u-1", None);
    assert!(harness.credentials().await.present(&key).is_none());
    // 位置重新解析，这次签发成功。
    assert_eq!(pat(&harness, ZAI_PLAN, None).await.unwrap(), "pat-1");
    assert_eq!(server.count("/api/biz/customer"), 2);

    issue_status.lock().unwrap().push(200);
    harness.advance(3_600_000);
    let message = auth_error(pat(&harness, ZAI_PLAN, None).await);
    assert!(message.starts_with("project_token_disabled"), "{message}");
}

#[tokio::test]
async fn transient_failures_keep_a_valid_token_until_it_expires() {
    let account = Account::new(true);
    let failing = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let server = Server::start({
        let (account, failing) = (account.clone(), failing.clone());
        move |request| {
            if request.path == ISSUE && failing.load(std::sync::atomic::Ordering::SeqCst) {
                Reply::json(503, &json!({}))
            } else {
                account.reply(request)
            }
        }
    })
    .await;
    let harness = Harness::new(&server.url()).await;
    zai_login(&harness, "at-zai").await;
    assert_eq!(pat(&harness, ZAI_PLAN, None).await.unwrap(), "pat-1");
    failing.store(true, std::sync::atomic::Ordering::SeqCst);
    // 过了刷新点、还没到期：签发 503，继续用旧令牌，5 秒内不再请求。
    harness.advance(3_600_000 - 60_000);
    assert_eq!(pat(&harness, ZAI_PLAN, None).await.unwrap(), "pat-1");
    assert_eq!(pat(&harness, ZAI_PLAN, None).await.unwrap(), "pat-1");
    assert_eq!(server.count(ISSUE), 2);
    // 到期之后：失败。
    harness.advance(60_000);
    let message = auth_error(pat(&harness, ZAI_PLAN, None).await);
    assert!(message.starts_with("project_token_"), "{message}");
}

#[tokio::test]
async fn a_rejected_business_jwt_is_not_exchanged_again() {
    // 修复线 F47：存的是业务 JWT，没有 OAuth token 可以重换（App 相同）；以前按 Node 重换一次再签发。
    let account = Account::new(true);
    let server = Server::start({
        let account = account.clone();
        move |request| {
            if request.path == ISSUE {
                Reply::json(401, &json!({"code": 401}))
            } else {
                account.reply(request)
            }
        }
    })
    .await;
    let harness = Harness::new(&server.url()).await;
    zai_login(&harness, "jwt-rejected").await;
    let message = auth_error(pat(&harness, ZAI_PLAN, None).await);
    assert!(
        message.starts_with("project_token_request_failed"),
        "{message}"
    );
    assert_eq!(server.count("/api/auth/z/login"), 0);
    assert_eq!(server.count(ISSUE), 1);
}

#[tokio::test]
async fn bigmodel_uses_the_access_token_directly() {
    let account = Account::new(true);
    let server = Server::start({
        let account = account.clone();
        move |request| account.reply(request)
    })
    .await;
    let harness = Harness::new(&server.url()).await;
    // App 的 bigmodel profile（本工程 F50 之前的登录写的 `key-…` id 也一样读）。
    harness
        .write_credentials(&json!({
            "oauth:bigmodel:user_info": r#"{"id":"key-abc","username":"dev","displayName":"Dev"}"#,
            "oauth:bigmodel:access_token": "Bearer at-big",
        }))
        .await;
    let token = pat(&harness, (BIGMODEL, "bigmodel"), None).await.unwrap();
    assert_eq!(token, "pat-1");
    let requests = server.requests();
    assert_eq!(server.count("/api/auth"), 0);
    // bigmodel 的 customerInfo 与 key 列表都用裸 token，签发用 Bearer；`Bearer ` 前缀先去掉。
    assert_eq!(requests[0].header("authorization"), Some("at-big"));
    assert_eq!(requests[1].header("authorization"), Some("at-big"));
    assert_eq!(requests[2].header("authorization"), Some("Bearer at-big"));
    let value: Value = requests[2].json();
    assert_eq!(value["clientType"], "zcode");
}

#[tokio::test]
async fn switching_accounts_mid_exchange_invalidates_the_token() {
    let account = Account::new(true);
    // 登录之后的第一个签发请求到达假服务时通知测试，回复停在闸门上：测试换完账号再放行（F123：以前固定停 300 毫秒，
    // 负载高时不够长）。
    let arrived = Arc::new(tokio::sync::Notify::new());
    let (gate, armed) = (Gate::default(), Arc::new(AtomicBool::new(false)));
    let server = Server::start({
        let (account, arrived, gate, armed) = (
            account.clone(),
            arrived.clone(),
            gate.clone(),
            armed.clone(),
        );
        move |request| {
            let reply = account.reply(request);
            if request.path == ISSUE && armed.swap(false, Ordering::SeqCst) {
                arrived.notify_one();
                reply.until(&gate)
            } else {
                reply
            }
        }
    })
    .await;
    let harness = Arc::new(Harness::new(&server.url()).await);
    zai_login(&harness, "at-zai").await;
    armed.store(true, Ordering::SeqCst);
    let pending = tokio::spawn({
        let harness = harness.clone();
        async move { pat(&harness, ZAI_PLAN, None).await }
    });
    arrived.notified().await;
    zai_login(&harness, "at-switched").await;
    gate.open();
    let message = auth_error(pending.await.unwrap());
    assert!(
        message.starts_with("project_token_scope_invalidated"),
        "{message}"
    );
}
