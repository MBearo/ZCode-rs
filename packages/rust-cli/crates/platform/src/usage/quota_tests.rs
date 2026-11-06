//! 一个计划的额度（platform.md §11.3、§11.4），经平台层对本地假服务：订阅摘要、quota/limit 与 MCP 额度的请求与整理。

use std::sync::Arc;

use rust_cli_kernel_api::model::ModelConnection;
use serde_json::{Value, json};

use super::*;
use crate::fake_server_tests::{Captured, Harness, Reply, Server, ZAI};
use crate::usage::tests::manual_login;
use crate::usage::{PlanTarget, mcp};

const QUOTA: &str = "/api/monitor/usage/quota/limit";
const SUBSCRIPTION: &str = "/api/biz/subscription/list";
const TEAM_DETAIL: &str = "/api/biz/team/subscribe/product/querySubscribeDetail";

const EAST_8: &(dyn Fn(u64) -> i32 + Sync) = &|_| 480;

fn ok(data: Value) -> Reply {
    Reply::json(200, &json!({"code": 200, "data": data}))
}

fn path(request: &Captured) -> &str {
    request.path.split('?').next().unwrap_or_default()
}

fn limits() -> Value {
    json!({"level": "pro", "limits": [
        {"type": "TOKENS_LIMIT", "unit": 3, "number": 5, "percentage": 30, "nextResetTime": 1_700_000_100_000_u64},
        {"type": "TIME_LIMIT", "unit": 5, "number": 1, "remaining": 88, "percentage": 12,
         "usageDetails": [{"modelCode": "search-prime", "displayName": " 搜索 ", "usage": 7}, {"usage": 1}]},
        {"type": "", "unit": 6},
        {"unit": 6}
    ]})
}

fn individual() -> PlanTarget {
    PlanTarget {
        provider: ZAI.to_owned(),
        family: Family::Zai,
        team: None,
    }
}

/// 有效的个人订阅：摘要的续费与到期照 App 的规则；quota/limit 用原样的 key（不加 Bearer）、不带 `type`；没有登录的
/// family 不问 MCP。
#[tokio::test]
async fn a_personal_plan_reads_the_subscription_and_the_quota() {
    let server = Server::start(|request| match path(request) {
        SUBSCRIPTION => ok(json!([{
            "productId": "coding-pro", "productName": "GLM Coding Pro", "status": "VALID",
            "inCurrentPeriod": true, "autoRenew": 1,
            "nextRenewTime": "2023-12-14 22:13:20", "valid": "2023-11-14 22:13:20-2023-12-14 22:13:20"
        }])),
        QUOTA => ok(limits()),
        _ => Reply::json(404, &json!({})),
    })
    .await;
    let harness = Harness::new(&server.url()).await;
    harness
        .write_credentials(&manual_login(ZAI, "sk-zai"))
        .await;
    let snapshot = harness.platform.plan_quota(&individual()).await.unwrap();
    assert_eq!(snapshot.at_ms, crate::fake_server_tests::NOW_MS);
    assert_eq!(snapshot.plan, PlanStatus::Available);
    // 测试时钟在 UTC：不带时区的时刻按 UTC。
    assert_eq!(snapshot.renews_ms, Some(1_702_592_000_000));
    assert_eq!(snapshot.ends_ms, Some(1_702_592_000_000));
    let quota = snapshot.quota.unwrap();
    assert_eq!(quota.level.as_deref(), Some("pro"));
    assert_eq!(quota.limits.len(), 2);
    assert_eq!(quota.limits[1].details[0].name.as_deref(), Some("搜索"));
    assert_eq!(snapshot.mcp, None);
    let requests = server.requests();
    let quota_request = requests.iter().find(|r| path(r) == QUOTA).unwrap();
    assert_eq!(quota_request.path, QUOTA);
    assert_eq!(quota_request.header("authorization"), Some("sk-zai"));
    assert_eq!(quota_request.header("bigmodel-organization"), None);
    let subscription = requests.iter().find(|r| path(r) == SUBSCRIPTION).unwrap();
    assert_eq!(subscription.header("authorization"), Some("sk-zai"));
}

/// 没有凭据的个人计划：不发请求，结论是“没有配置”；订阅不明而有凭据时整次失败（不当作空快照）；没有订阅是 no plan。
#[tokio::test]
async fn subscription_outcomes_follow_the_app() {
    let replies = Arc::new(std::sync::Mutex::new(Reply::json(500, &json!({}))));
    let script = replies.clone();
    let server = Server::start(move |request| match path(request) {
        SUBSCRIPTION => {
            let reply = script.lock().unwrap();
            Reply::json(
                reply.status,
                &serde_json::from_str::<Value>(&reply.body).unwrap(),
            )
        }
        QUOTA => ok(limits()),
        _ => Reply::json(404, &json!({})),
    })
    .await;
    let harness = Harness::new(&server.url()).await;
    let none = harness.platform.plan_quota(&individual()).await.unwrap();
    assert_eq!(none.plan, PlanStatus::NotConfigured);
    assert_eq!(none.quota, None);
    assert!(server.requests().is_empty());

    harness
        .write_credentials(&manual_login(ZAI, "sk-zai"))
        .await;
    assert_eq!(
        harness
            .platform
            .plan_quota(&individual())
            .await
            .unwrap_err(),
        "Coding Plan entitlement refresh failed"
    );
    *replies.lock().unwrap() = ok(json!([]));
    let empty = harness.platform.plan_quota(&individual()).await.unwrap();
    assert_eq!(empty.plan, PlanStatus::NoPlan);
    // 额度照样查（App 相同；host 在不是 ok 时不显示池）。
    assert!(empty.quota.is_some());
}

/// 团队计划：订阅详情用登录 token 与两个团队头；quota/limit 加 `type=2` 与团队头；过期的团队是 TeamExpired。
#[tokio::test]
async fn a_team_plan_scopes_every_request() {
    let status = Arc::new(std::sync::Mutex::new("EFFECTIVE"));
    let script = status.clone();
    let server = Server::start(move |request| match path(request) {
        TEAM_DETAIL => ok(json!({
            "hasSubscription": true, "status": *script.lock().unwrap(), "memberGrantStatus": "VALID",
            "subscribeEndTime": "2023-12-31T00:00:00Z"
        })),
        QUOTA => ok(limits()),
        _ => Reply::json(404, &json!({})),
    })
    .await;
    let harness = Harness::new(&server.url()).await;
    harness
        .write_credentials(&json!({"oauth:bigmodel:access_token": "login-token"}))
        .await;
    let target = PlanTarget {
        provider: "account:bigmodel-team-coding-plan".to_owned(),
        family: Family::Bigmodel,
        team: Some(ModelConnection {
            product_id: "prod".to_owned(),
            organization_id: "org-1".to_owned(),
            project_id: "p-1".to_owned(),
        }),
    };
    // 换不到项目令牌（没有 profile id）：订阅的结论照样留着，额度为空。
    let snapshot = harness.platform.plan_quota(&target).await.unwrap();
    assert_eq!(snapshot.plan, PlanStatus::Available);
    assert_eq!(snapshot.ends_ms, Some(1_703_980_800_000));
    assert_eq!(snapshot.quota, None);
    let detail = server
        .requests()
        .into_iter()
        .find(|r| path(r) == TEAM_DETAIL)
        .unwrap();
    assert_eq!(detail.header("authorization"), Some("login-token"));
    assert_eq!(detail.header("bigmodel-organization"), Some("org-1"));
    assert_eq!(detail.header("bigmodel-project"), Some("p-1"));
    *status.lock().unwrap() = "EXPIRED";
    let expired = harness.platform.plan_quota(&target).await.unwrap();
    assert_eq!(expired.plan, PlanStatus::TeamExpired);
}

/// quota/limit 的团队请求：`type=2`，覆盖地址的原有参数保留。
#[tokio::test]
async fn the_team_quota_request_carries_type_two() {
    let server = Server::start(|request| match path(request) {
        "/custom/quota" => ok(limits()),
        _ => Reply::json(404, &json!({})),
    })
    .await;
    let harness = Harness::new(&server.url()).await;
    let http = Http::with_limit(harness_egress().await, 1 << 20);
    let url = Url::parse(&format!("{}/custom/quota?x=1", server.url())).unwrap();
    let team = ModelConnection {
        product_id: "prod".to_owned(),
        organization_id: "org-1".to_owned(),
        project_id: "p-1".to_owned(),
    };
    let quota = fetch_quota(&http, url, &RedactedString::new("pat"), Some(&team))
        .await
        .unwrap();
    assert_eq!(quota.limits.len(), 2);
    let request = &server.requests()[0];
    assert_eq!(request.path, "/custom/quota?x=1&type=2");
    assert_eq!(request.header("authorization"), Some("pat"));
    assert_eq!(request.header("bigmodel-project"), Some("p-1"));
    drop(harness);
}

async fn harness_egress() -> Arc<rust_cli_net::Egress> {
    Arc::new(
        rust_cli_net::Egress::new(rust_cli_net::NetworkPolicy::default())
            .await
            .unwrap(),
    )
}

/// 当前登录的 family 才问 MCP：zcodejwttoken 与登录 token 都用 Bearer；个人的范围头是 PERSONAL。
#[tokio::test]
async fn the_mcp_quota_follows_the_active_login() {
    let server = Server::start(|request| match path(request) {
        "/api/v1/mcp/usage" => Reply::json(
            200,
            &json!({"code": 0, "data": {
                "server_time": 1, "next_refresh_at": 1_700_003_600,
                "total_usage": {"used": 25, "limit": 100, "remaining": 75}
            }}),
        ),
        SUBSCRIPTION => {
            ok(json!([{"productId": "coding", "status": "VALID", "inCurrentPeriod": true}]))
        }
        QUOTA => ok(limits()),
        _ => Reply::json(404, &json!({})),
    })
    .await;
    let harness = Harness::new(&server.url()).await;
    let mut login = manual_login(ZAI, "sk-zai");
    login["oauth:active_provider"] = json!("zai");
    login["oauth:zai:access_token"] = json!("login-token");
    login["zcodejwttoken"] = json!("jwt");
    harness.write_credentials(&login).await;
    let snapshot = harness.platform.plan_quota(&individual()).await.unwrap();
    let mcp = snapshot.mcp.unwrap();
    assert_eq!(mcp.kind, mcp::MCP_LIMIT);
    assert_eq!(mcp.percentage, Some(25.0));
    assert_eq!(mcp.next_reset_ms, Some(1_700_003_600_000.0));
    let request = server
        .requests()
        .into_iter()
        .find(|r| path(r) == "/api/v1/mcp/usage")
        .unwrap();
    assert_eq!(request.header("authorization"), Some("Bearer jwt"));
    assert_eq!(
        request.header("x-bigmodel-authorization"),
        Some("Bearer login-token")
    );
    assert_eq!(request.header("bigmodel-target-type"), Some("PERSONAL"));
    // 另一个 family 登录着：不问。
    login["oauth:active_provider"] = json!("bigmodel");
    harness.write_credentials(&login).await;
    let other = harness.platform.plan_quota(&individual()).await.unwrap();
    assert_eq!(other.mcp, None);
}

#[test]
fn mcp_totals_map_like_the_app() {
    let reply = |total: Value| json!({"code": 0, "data": {"server_time": 1, "total_usage": total}});
    let clamped = mcp::aggregate(&reply(json!({"used": 1, "limit": 10, "remaining": 20}))).unwrap();
    assert_eq!(
        (clamped.remaining, clamped.percentage),
        (Some(10.0), Some(0.0))
    );
    assert_eq!(clamped.next_reset_ms, None);
    assert_eq!(
        mcp::aggregate(&reply(json!({"used": 1, "limit": 0, "remaining": 0}))),
        None
    );
    assert_eq!(
        mcp::aggregate(&reply(json!({"limit": 10, "remaining": 0}))),
        None
    );
    assert_eq!(mcp::aggregate(&json!({"code": 1, "data": {}})), None);
}

/// 续费与到期：自动续费时 `nextRenewTime` 是续费、`valid` 的最后一个日期是到期；否则 `nextRenewTime` 是到期。
#[test]
fn subscription_dates_follow_the_app() {
    let auto = json!({"autoRenew": true, "nextRenewTime": "2026-11-30 23:59:59",
                      "valid": "2026-10-30 00:00:00-2026-12-31 23:59:59"});
    assert_eq!(
        personal_dates(&auto, EAST_8),
        (Some(1_796_054_399_000), Some(1_798_732_799_000))
    );
    let once = json!({"autoRenew": 0, "nextRenewTime": "2026-11-30 23:59:59", "valid": "x"});
    assert_eq!(
        personal_dates(&once, EAST_8),
        (None, Some(1_796_054_399_000))
    );
    let dates = json!({"nextRenewTime": "soon", "valid": "2026-10-30 至 2026-12-31"});
    assert_eq!(
        personal_dates(&dates, EAST_8),
        (None, Some(1_798_675_200_000))
    );
    assert_eq!(
        last_date("2026-10-30T01:02:03 / 2026-11-30 01:02"),
        Some("2026-11-30")
    );
    assert_eq!(last_date("none"), None);
}

#[test]
fn limits_keep_only_typed_entries() {
    let limits = normalize_limits(&limits()["limits"]);
    assert_eq!(limits.len(), 2);
    assert_eq!(limits[0].kind, "TOKENS_LIMIT");
    assert_eq!(limits[0].unit, Some(3.0));
    assert_eq!(limits[0].remaining, None);
    assert_eq!(limits[1].remaining, Some(88.0));
    assert_eq!(limits[1].details.len(), 1);
    assert_eq!(limits[1].details[0].usage, 7.0);
    assert!(normalize_limits(&json!(null)).is_empty());
}

/// 查询途中账号换了（登录 token 变了）：这一次作废（App `assertCurrent`），不把旧账号的额度交出去。
#[tokio::test]
async fn an_account_change_during_the_request_fails_it() {
    let arrived = Arc::new(tokio::sync::Notify::new());
    let signal = arrived.clone();
    let server = Server::start(move |request| match path(request) {
        SUBSCRIPTION => {
            ok(json!([{"productId": "coding", "status": "VALID", "inCurrentPeriod": true}]))
        }
        QUOTA => {
            signal.notify_one();
            ok(limits()).after(300)
        }
        _ => Reply::json(404, &json!({})),
    })
    .await;
    let harness = Harness::new(&server.url()).await;
    let mut login = manual_login(ZAI, "sk-zai");
    harness.write_credentials(&login).await;
    let target = individual();
    let (result, ()) = tokio::join!(harness.platform.plan_quota(&target), async {
        arrived.notified().await;
        login["oauth:zai:access_token"] = json!("another-account");
        harness.write_credentials(&login).await;
    });
    assert_eq!(
        result.unwrap_err(),
        "the account changed during the request"
    );
}
