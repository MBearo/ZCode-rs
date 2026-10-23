//! 个人订阅列表的判定（platform.md §5.7）与 App 对拍：`app-login-golden.json` 的 `subscriptions` 一节是 App
//! `validateCodingPlanProviderAvailability`（个人）对每种回复的结论与请求，本工程对同样的回复（本地假服务）给出
//! 同样的结论、发出同样的请求。

use std::sync::Arc;

use rust_cli_net::{Egress, NetworkPolicy};
use serde_json::Value;

use super::subscription;
use crate::fake_server_tests::{Reply, Server};
use crate::team::{Availability, Reason};
use crate::token::http::Http;

fn golden() -> Value {
    serde_json::from_str(include_str!("../../tests/fixtures/app-login-golden.json")).unwrap()
}

/// App 的结论（`{kind, reason?}`）换成本工程的。
fn app_result(result: &Value) -> Availability {
    match (result["kind"].as_str(), result["reason"].as_str()) {
        (Some("available"), _) => Availability::Available,
        (Some("unavailable"), Some("coding_plan_auth_failed")) => {
            Availability::Unavailable(Reason::AuthFailed)
        }
        (Some("unavailable"), Some("coding_plan_not_entitled")) => {
            Availability::Unavailable(Reason::NotEntitled)
        }
        (Some("unknown"), _) => Availability::Unknown,
        other => panic!("unexpected App result {other:?}"),
    }
}

#[tokio::test]
async fn subscription_results_match_the_app() {
    let golden = golden();
    let cases = golden["subscriptions"].as_object().unwrap();
    assert!(cases.len() >= 30);
    let http = Http::new(Arc::new(
        Egress::new(NetworkPolicy::default()).await.unwrap(),
    ));
    for (name, case) in cases {
        for family in ["zai", "bigmodel"] {
            let expected = app_result(&case[family]["result"]);
            let script = &case["script"];
            if script.is_null() {
                // 没有项目令牌：App 不发请求，结论 unknown（本工程在换取失败时直接给 unknown，见 `PersonalPlans`）。
                assert_eq!(expected, Availability::Unknown, "{name}");
                assert!(case[family]["requests"].as_array().unwrap().is_empty());
                continue;
            }
            let status = u16::try_from(script["status"].as_u64().unwrap()).unwrap();
            let body = script["body"].as_str().unwrap().to_owned();
            let server = Server::start(move |_| Reply {
                status,
                body: body.clone(),
                delay: std::time::Duration::ZERO,
            })
            .await;
            let actual = subscription(&http, &server.url(), "pat-1").await;
            assert_eq!(actual, expected, "{name} ({family})");
            // 请求与 App 相同：一个 GET，路径与鉴权头（项目令牌，不带 Bearer）。
            let app = &case[family]["requests"];
            let requests = server.requests();
            assert_eq!(requests.len(), 1, "{name}");
            assert_eq!(requests[0].method, app[0]["method"], "{name}");
            assert_eq!(requests[0].path, app[0]["path"], "{name}");
            assert_eq!(
                requests[0].header("authorization"),
                app[0]["authorization"].as_str(),
                "{name}"
            );
        }
    }
}
