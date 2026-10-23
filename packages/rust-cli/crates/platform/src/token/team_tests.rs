//! 团队与个人（non-team）项目令牌的换取（platform.md §5.4，工作线 F47）：请求序列、鉴权与团队头、请求体、
//! 项目位置键与值，与 App 的 `ProjectAccessTokenClient` 逐项相同（golden 由
//! `scripts/rust-cli/team-plan-golden.mts` 生成），对本地假服务。

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use rust_cli_net::{Egress, NetworkPolicy};
use serde_json::{Value, json};

use super::{AccountOrigins, Family, ProjectTokens, TeamProject, TokenError, TokenRequest};
use crate::fake_server_tests::{Captured, Harness, NOW_MS, Reply, Server};
use crate::test_support::FixedClock;

/// 按 golden 一行的脚本回复：customerInfo、key 列表、创建、签发。
fn reply(customer: &Value, keys: &Value, request: &Captured) -> Reply {
    let ok = |data: Value| Reply::json(200, &json!({"code": 200, "data": data}));
    let path = request.path.as_str();
    match request.method.as_str() {
        "GET" if path == "/api/biz/customer/getCustomerInfo" => ok(customer.clone()),
        "GET" if path.ends_with("/api_keys") => ok(keys.clone()),
        "POST" if path.ends_with("/api_keys") => {
            let body = request.json();
            let mut key = json!({"name": body["name"], "apiKey": "key-new"});
            if let Some(kind) = body.get("keyType") {
                key["keyType"] = kind.clone();
            }
            ok(key)
        }
        "POST" if path.ends_with("/access_tokens") => {
            let now = NOW_MS / 1000;
            ok(json!({
                "accessToken": "pat-1",
                "tokenType": "Bearer",
                "expiresIn": 3600,
                "expiresAt": now + 3600,
            }))
        }
        _ => Reply::json(404, &json!({"code": 404})),
    }
}

fn recorded(request: &Captured) -> Value {
    let mut headers = serde_json::Map::new();
    for name in [
        "authorization",
        "bigmodel-organization",
        "bigmodel-project",
        "content-type",
    ] {
        if let Some(value) = request.header(name) {
            headers.insert(name.to_owned(), json!(value));
        }
    }
    let body = if request.body.is_empty() {
        Value::Null
    } else {
        request.json()
    };
    json!({"method": request.method, "path": request.path, "headers": headers, "body": body})
}

fn team_of(input: &Value) -> Option<TeamProject> {
    input["team"].as_object().map(|team| TeamProject {
        organization_id: team["organizationId"].as_str().unwrap().to_owned(),
        project_id: team["projectId"].as_str().unwrap().to_owned(),
    })
}

#[tokio::test]
async fn team_and_personal_exchanges_match_the_app() {
    let golden = crate::test_support::golden();
    let customer = golden["tokenCustomer"].clone();
    let cases = golden["tokens"].as_object().unwrap();
    assert_eq!(cases.len(), 4);
    for (name, case) in cases {
        let input = &case["input"];
        let keys = input["keys"].clone();
        let server = Server::start({
            let customer = customer.clone();
            move |request| reply(&customer, &keys, request)
        })
        .await;
        let harness = Harness::new(&server.url()).await;
        let family = Family::parse(input["family"].as_str().unwrap()).unwrap();
        let tokens = ProjectTokens::new(
            Arc::new(Egress::new(NetworkPolicy::default()).await.unwrap()),
            Arc::new(FixedClock(AtomicU64::new(NOW_MS))),
            AccountOrigins {
                zcode_api: String::new(),
                zai_api: server.url(),
                bigmodel_api: server.url(),
                bigmodel_team_plans: format!("{}/coding-plan/team/plans", server.url()),
                // App 的 `ZCODE_VERSION` 在源码检出里是 0.0.0-dev。
                client_version: "0.0.0-dev".to_owned(),
            },
        );
        let team = team_of(input);
        let login_token = match family {
            Family::Zai => "jwt-zai",
            Family::Bigmodel => "at-big",
        };
        let store = harness.store();
        let request = TokenRequest {
            family,
            login_token,
            account: "u-1",
            team: team.as_ref(),
            rejected: None,
        };
        let material = tokens.resolve(request, &store).await.unwrap();
        let expected = &case["material"];
        assert_eq!(material.token.expose(), expected["token"], "{name}");
        assert_eq!(
            material.location.organization_id, expected["organizationId"],
            "{name}"
        );
        assert_eq!(
            material.location.project_id, expected["projectId"],
            "{name}"
        );
        assert_eq!(material.location.api_key_id, expected["apiKeyId"], "{name}");

        let ours: Vec<Value> = server.requests().iter().map(recorded).collect();
        assert_eq!(Value::Array(ours), case["requests"], "{name}");

        // 位置键：同一个 origin 算出的键与 App 逐字节相同；存进凭据的值与 App 相同。
        let saved = &case["saved"][0];
        let origin = case["origin"].as_str().unwrap();
        assert_eq!(
            super::location::storage_key(origin, family, "u-1", team.as_ref()),
            saved[0],
            "{name}"
        );
        let key = super::location::storage_key(&server.url(), family, "u-1", team.as_ref());
        let value = harness.credentials().await.present(&key).unwrap();
        assert_eq!(value.expose(), saved[1], "{name}");
    }
}

#[tokio::test]
async fn team_locations_are_checked_and_re_resolved() {
    let golden = crate::test_support::golden();
    let customer = golden["tokenCustomer"].clone();
    let server = Server::start(move |request| {
        reply(
            &customer,
            &json!([{"name": "zcode-team-api-key", "apiKey": "key-team", "keyType": 2}]),
            request,
        )
    })
    .await;
    let harness = Harness::new(&server.url()).await;
    let tokens = ProjectTokens::new(
        Arc::new(Egress::new(NetworkPolicy::default()).await.unwrap()),
        Arc::new(FixedClock(AtomicU64::new(NOW_MS))),
        AccountOrigins {
            zcode_api: String::new(),
            zai_api: server.url(),
            bigmodel_api: server.url(),
            bigmodel_team_plans: format!("{}/coding-plan/team/plans", server.url()),
            client_version: "0.1.0-test".to_owned(),
        },
    );
    let team = TeamProject {
        organization_id: "org-t".to_owned(),
        project_id: "p-t".to_owned(),
    };
    // 存着的位置指向别的项目（被改过或损坏）：读回核对不过，重新解析并覆盖。
    let key = super::location::storage_key(&server.url(), Family::Bigmodel, "u-1", Some(&team));
    harness
        .write_credentials(&json!({
            key.clone(): r#"{"organizationId":"org-1","projectId":"p-own","apiKeyId":"key-x"}"#,
        }))
        .await;
    let store = harness.store();
    fn request(team: Option<&TeamProject>) -> TokenRequest<'_> {
        TokenRequest {
            family: Family::Bigmodel,
            login_token: "at-big",
            account: "u-1",
            team,
            rejected: None,
        }
    }
    let material = tokens.resolve(request(Some(&team)), &store).await.unwrap();
    assert_eq!(material.location.api_key_id, "key-team");
    assert_eq!(server.count("/api/biz/customer"), 1);
    let saved = harness.credentials().await.present(&key).unwrap();
    assert_eq!(
        saved.expose(),
        r#"{"organizationId":"org-t","projectId":"p-t","apiKeyId":"key-team"}"#
    );

    // 不在 customerInfo 里的团队项目：project_token_project_unavailable，不建 key。
    let gone = TeamProject {
        organization_id: "org-t".to_owned(),
        project_id: "p-gone".to_owned(),
    };
    let error = tokens
        .resolve(request(Some(&gone)), &store)
        .await
        .unwrap_err();
    assert_eq!(error, TokenError::ProjectUnavailable);
    assert_eq!(
        server.count("/api/biz/v1/organization/org-t/projects/p-gone"),
        0
    );

    // 个人与团队在同一个 login token 下各有各的位置与 PAT（单飞、缓存按账号与项目分开）。
    let personal = tokens.resolve(request(None), &store).await.unwrap();
    assert_eq!(personal.location.project_id, "p-own");
    let again = tokens.resolve(request(Some(&team)), &store).await.unwrap();
    assert_eq!(again.location.project_id, "p-t");
    assert_eq!(
        server.count("/api/biz/v1/organization/org-t/projects/p-t/api_keys/key-team"),
        1
    );
}

#[tokio::test]
async fn invalid_team_keys_are_rejected() {
    // 创建回来的条目名字或类型不对：project_token_invalid_response（App 相同）。
    let golden = crate::test_support::golden();
    let customer = golden["tokenCustomer"].clone();
    let server = Server::start(move |request| {
        if request.method == "POST" && request.path.ends_with("/api_keys") {
            Reply::json(
                200,
                &json!({"code": 200, "data": {"name": "zcode-team-api-key", "apiKey": "k", "keyType": 1}}),
            )
        } else {
            reply(&customer, &json!([]), request)
        }
    })
    .await;
    let harness = Harness::new(&server.url()).await;
    let tokens = ProjectTokens::new(
        Arc::new(Egress::new(NetworkPolicy::default()).await.unwrap()),
        Arc::new(FixedClock(AtomicU64::new(NOW_MS))),
        AccountOrigins {
            zcode_api: String::new(),
            zai_api: server.url(),
            bigmodel_api: server.url(),
            bigmodel_team_plans: format!("{}/coding-plan/team/plans", server.url()),
            client_version: "0.1.0-test".to_owned(),
        },
    );
    let team = TeamProject {
        organization_id: "org-t".to_owned(),
        project_id: "p-t".to_owned(),
    };
    let request = TokenRequest {
        family: Family::Zai,
        login_token: "jwt-zai",
        account: "u-1",
        team: Some(&team),
        rejected: None,
    };
    let error = tokens.resolve(request, &harness.store()).await.unwrap_err();
    assert_eq!(error, TokenError::InvalidResponse);
}
