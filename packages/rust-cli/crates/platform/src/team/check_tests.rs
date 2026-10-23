//! 团队权益的判定表（platform.md §9.2）：每一行对本地假服务，与 App 的
//! `validateZaiAccountProviderAvailability` / `validateBigModelAccountProviderAvailability` 的结论、请求路径与
//! 鉴权头逐项相同（golden 由 `scripts/rust-cli/team-plan-golden.mts` 生成）。

use std::sync::Arc;

use rust_cli_net::{Egress, NetworkPolicy};
use serde_json::{Value, json};

use rust_cli_kernel_api::model::ModelConnection;

use super::check::{Detail, detail, verify};
use super::list::customer_info;
use super::{Availability, Reason};
use crate::fake_server_tests::{Reply, Server};
use crate::token::Family;
use crate::token::http::Http;

/// App `CodingPlanAvailabilityResult` 的写法（团队的过期 / 未分配在 App 里并成 not_entitled）。
fn as_app(availability: Availability) -> Value {
    let reason = |reason: Reason| match reason {
        Reason::AuthFailed => "coding_plan_auth_failed",
        Reason::NotEntitled | Reason::Expired | Reason::Unassigned => "coding_plan_not_entitled",
    };
    match availability {
        Availability::Available => json!({"kind": "available"}),
        Availability::Unknown => json!({"kind": "unknown"}),
        Availability::Unavailable(why) => json!({"kind": "unavailable", "reason": reason(why)}),
    }
}

fn team() -> ModelConnection {
    ModelConnection {
        product_id: "prod-1".to_owned(),
        organization_id: "org-t".to_owned(),
        project_id: "p-t".to_owned(),
    }
}

fn scripted(script: &Value, default: Value) -> Reply {
    let (status, body) = match script {
        Value::Null => (200, default),
        script => (
            script["status"].as_u64().unwrap() as u16,
            script["body"].clone(),
        ),
    };
    Reply::json(status, &body)
}

/// 跑一行：customerInfo 缺省回 App 夹具里的团队项目，订阅详情缺省 404。
async fn run(family: Family, script: &Value) -> (Availability, Vec<Value>) {
    let script = script.clone();
    let server = Server::start(move |request| {
        if request.path == "/api/biz/customer/getCustomerInfo" {
            let customer = json!({"code": 200, "data": {"organizations": [
                {"organizationId": " org-t ", "projects": [{"projectId": "p-t", "projectType": 2}]}
            ]}});
            scripted(&script["customer"], customer)
        } else {
            match &script["detail"] {
                Value::Null => Reply::json(404, &json!({})),
                detail => scripted(detail, Value::Null),
            }
        }
    })
    .await;
    let http = Http::new(Arc::new(
        Egress::new(NetworkPolicy::default()).await.unwrap(),
    ));
    let team = team();
    let token = match family {
        Family::Zai => "jwt-zai",
        Family::Bigmodel => "at-big",
    };
    let origin = server.url();
    // App 的一次校验：customerInfo 核对项目，再查订阅详情（本工程的 customerInfo 由列表一起取，D-F49-1）。
    let customer = customer_info(&http, &origin, token).await;
    let availability = match verify(&customer, &team) {
        Ok(organization) => {
            let input = Detail {
                origin: &origin,
                family,
                team: &team,
                token,
                organization: &organization,
            };
            detail(&http, input).await
        }
        Err(availability) => availability,
    };
    let requests = server
        .requests()
        .iter()
        .map(|request| {
            let mut headers = serde_json::Map::new();
            for name in ["authorization", "bigmodel-organization", "bigmodel-project"] {
                if let Some(value) = request.header(name) {
                    headers.insert(name.to_owned(), json!(value));
                }
            }
            json!({"method": request.method, "path": request.path, "headers": headers})
        })
        .collect();
    (availability, requests)
}

/// App 的请求：路径的 `//` 按单个 `/`（zai customerInfo 的地址 App 多一个 `/`，D-F47-5）；只比鉴权与团队头
/// （本工程的 GET 都带 `content-type`，App 的订阅详情没带）。
fn app_requests(requests: &Value) -> Vec<Value> {
    requests
        .as_array()
        .unwrap()
        .iter()
        .map(|request| {
            let mut headers = request["headers"].as_object().unwrap().clone();
            headers.remove("content-type");
            json!({
                "method": request["method"],
                "path": request["path"].as_str().unwrap().replace("//", "/"),
                "headers": headers,
            })
        })
        .collect()
}

#[tokio::test]
async fn every_row_of_the_entitlement_table_matches_the_app() {
    let golden = crate::test_support::golden();
    let cases = golden["entitlements"].as_object().unwrap();
    assert!(cases.len() >= 20);
    for (name, case) in cases {
        for family in [Family::Zai, Family::Bigmodel] {
            let expected = &case[family.as_str()];
            let (availability, requests) = run(family, &case["script"]).await;
            assert_eq!(
                as_app(availability),
                expected["availability"],
                "{name} {family:?}"
            );
            assert_eq!(
                requests,
                app_requests(&expected["requests"]),
                "{name} {family:?}"
            );
        }
    }
}

#[tokio::test]
async fn expired_and_unassigned_keep_their_reason() {
    let golden = crate::test_support::golden();
    let reasons = golden["detailReasons"].as_object().unwrap();
    for (name, expected) in reasons {
        let (availability, _) =
            run(Family::Bigmodel, &golden["entitlements"][name]["script"]).await;
        let ours = match availability {
            Availability::Available => json!({"kind": "available", "reason": null}),
            Availability::Unknown => json!({"kind": "unknown", "reason": null}),
            Availability::Unavailable(Reason::Expired) => {
                json!({"kind": "unavailable", "reason": "expired"})
            }
            Availability::Unavailable(Reason::Unassigned) => {
                json!({"kind": "unavailable", "reason": "unassigned"})
            }
            Availability::Unavailable(_) => json!({"kind": "unavailable", "reason": null}),
        };
        assert_eq!(&ours, expected, "{name}");
    }
    assert_eq!(
        super::notice_text(Reason::Expired, "https://z.ai/manage-apikey/subscription").as_deref(),
        Some(
            "Team plan expired. Contact your team administrator. Manage team plan: \
             https://z.ai/manage-apikey/subscription"
        )
    );
    assert_eq!(
        super::notice_text(
            Reason::Unassigned,
            "https://bigmodel.cn/coding-plan/team/plans"
        )
        .as_deref(),
        Some(
            "Team plan not assigned. Contact your team admin. Manage team plan: \
             https://bigmodel.cn/coding-plan/team/plans"
        )
    );
    assert_eq!(super::notice_text(Reason::NotEntitled, "x"), None);
}
